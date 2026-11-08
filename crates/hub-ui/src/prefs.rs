//! Préférences de l'interface (par utilisateur, hors config du daemon) : `ui.toml`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Nom du produit (dossier des préférences, entrée de démarrage automatique).
const APP_NAME: &str = "ControlAnything";
/// Ancien nom : ses préférences et son entrée de démarrage sont reprises.
const LEGACY_APP_NAME: &str = "PC Hub";

/// Unité d'affichage des températures ; les valeurs restent en °C en interne.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TempUnit {
    #[default]
    Celsius,
    Fahrenheit,
}

impl TempUnit {
    /// Température absolue (°C) dans l'unité affichée.
    pub fn convert(self, celsius: f32) -> f32 {
        match self {
            Self::Celsius => celsius,
            Self::Fahrenheit => celsius * 1.8 + 32.0,
        }
    }

    /// Écart de température (hystérésis…) : pas de décalage de 32 °F.
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

    /// « 51.2 °C » / « 124.2 °F ».
    pub fn format(self, celsius: f32, decimals: usize) -> String {
        format!("{:.decimals$} {}", self.convert(celsius), self.symbol())
    }

    /// Écart formaté (« 2.0 °C » → « 3.6 °F »).
    pub fn format_delta(self, celsius: f32, decimals: usize) -> String {
        format!("{:.decimals$} {}", self.delta(celsius), self.symbol())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub launch_at_startup: bool,
    /// Lancé avec `--minimized` (démarrage de session) : rester dans la zone de notification.
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
    /// `…/ControlAnything/config/ui.toml` (Windows : `%APPDATA%\ControlAnything\config\ui.toml`).
    fn path() -> Option<PathBuf> {
        prefs_path(APP_NAME)
    }

    /// Charge `ui.toml` ; fichier absent ou invalide → valeurs par défaut.
    /// Au premier lancement sous le nouveau nom, reprend le `ui.toml` de « PC Hub ».
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            tracing::warn!(
                "dossier de configuration utilisateur introuvable : préférences par défaut"
            );
            return Self::default();
        };
        if let Some(legacy) = prefs_path(LEGACY_APP_NAME) {
            match migrate_file(&legacy, &path) {
                Ok(true) => tracing::info!("préférences reprises de {}", legacy.display()),
                Ok(false) => {}
                Err(err) => tracing::warn!("reprise de {} impossible : {err}", legacy.display()),
            }
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|err| {
                tracing::warn!(
                    "{} invalide ({err}) : préférences par défaut",
                    path.display()
                );
                Self::default()
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                tracing::warn!(
                    "lecture de {} impossible ({err}) : préférences par défaut",
                    path.display()
                );
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path().ok_or("Dossier de configuration utilisateur introuvable.")?;
        let text = toml::to_string(self).map_err(|err| err.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
        }
        std::fs::write(&path, text)
            .map_err(|err| format!("Écriture de {} impossible : {err}", path.display()))
    }
}

fn prefs_path(app_name: &str) -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", app_name).map(|d| d.config_dir().join("ui.toml"))
}

/// Copie `legacy` vers `target` si `target` n'existe pas encore. `true` si la copie a eu lieu.
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

/// Retire l'entrée de démarrage « PC Hub » ; si elle était active, la recrée sous le nouveau nom.
/// Renvoie `true` si le démarrage automatique a été réactivé.
pub fn migrate_legacy_autostart() -> bool {
    // Une build de dev ne doit pas détourner l'entrée de démarrage vers son dossier `target`.
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
        tracing::warn!("ancienne entrée de démarrage « {LEGACY_APP_NAME} » : {err}");
        return false;
    }
    set_autostart(true)
        .inspect_err(|err| tracing::warn!("{err}"))
        .is_ok()
}

/// Entrée de démarrage automatique : l'exécutable courant, lancé dans la zone de notification.
fn auto_launch(app_name: &str) -> Result<auto_launch::AutoLaunch, String> {
    let exe = std::env::current_exe().map_err(|err| err.to_string())?;
    auto_launch::AutoLaunchBuilder::new()
        .set_app_name(app_name)
        .set_app_path(&exe.to_string_lossy())
        .set_args(&["--minimized"])
        // Clé HKCU : pas besoin des droits administrateur.
        .set_windows_enable_mode(auto_launch::WindowsEnableMode::CurrentUser)
        .build()
        .map_err(|err| err.to_string())
}

/// Active ou retire le lancement à l'ouverture de session.
pub fn set_autostart(enabled: bool) -> Result<(), String> {
    let launcher = auto_launch(APP_NAME)?;
    let result = if enabled {
        launcher.enable()
    } else {
        launcher.disable()
    };
    result.map_err(|err| format!("Démarrage automatique : {err}"))
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
        // Clés absentes : valeurs par défaut.
        let partial: Prefs = toml::from_str("close_to_tray = false").unwrap_or_default();
        assert!(!partial.close_to_tray && partial.start_minimized);
    }

    #[test]
    fn autostart_entry_uses_minimized_flag() {
        // Construire l'entrée ne touche pas au registre.
        let launcher = auto_launch(APP_NAME).expect("entrée de démarrage");
        assert_eq!(launcher.get_app_name(), "ControlAnything");
        assert_eq!(launcher.get_args(), ["--minimized"]);
    }

    #[test]
    fn legacy_prefs_are_copied_once() {
        let dir = std::env::temp_dir().join(format!("controlanything-prefs-{}", std::process::id()));
        let legacy = dir.join("legacy").join("ui.toml");
        let target = dir.join("new").join("config").join("ui.toml");
        let _ = std::fs::remove_dir_all(&dir);
        // Pas d'ancien fichier : rien à faire.
        assert!(!migrate_file(&legacy, &target).expect("migration"));
        std::fs::create_dir_all(legacy.parent().expect("parent")).expect("dossier");
        std::fs::write(&legacy, "hotkeys = false").expect("écriture");
        assert!(migrate_file(&legacy, &target).expect("migration"));
        assert_eq!(std::fs::read_to_string(&target).expect("lecture"), "hotkeys = false");
        // Le nouveau fichier existe : jamais écrasé.
        std::fs::write(&legacy, "hotkeys = true").expect("écriture");
        assert!(!migrate_file(&legacy, &target).expect("migration"));
        assert_eq!(std::fs::read_to_string(&target).expect("lecture"), "hotkeys = false");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
