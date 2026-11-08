//! Persistance de `config.toml` : chargement tolérant, sauvegarde atomique.

use anyhow::Context;
use hub_core::config::Config;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Dossier de l'application sous `%ProgramData%`.
const APP_DIR: &str = "ControlAnything";
/// Dossier de l'ancien nom « PC Hub », repris une fois par [`migrate_legacy`].
const LEGACY_DIR: &str = "PC Hub";

/// Emplacement de la config système du daemon.
pub fn config_path() -> PathBuf {
    if cfg!(windows) {
        windows_path(std::env::var_os("ProgramData"), APP_DIR)
    } else {
        PathBuf::from("/etc/controlanything/config.toml")
    }
}

/// `%ProgramData%\<dir>\config.toml`, avec repli si la variable manque.
fn windows_path(program_data: Option<OsString>, dir: &str) -> PathBuf {
    let base = program_data.map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from);
    base.join(dir).join("config.toml")
}

/// Windows : copie l'ancienne config `%ProgramData%\PC Hub\config.toml` si la nouvelle manque.
/// L'ancienne est laissée en place (rien n'est jamais supprimé).
pub fn migrate_legacy(path: &Path) {
    if !cfg!(windows) {
        return;
    }
    let legacy = windows_path(std::env::var_os("ProgramData"), LEGACY_DIR);
    if !should_migrate(path, &legacy) {
        return;
    }
    let copied = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::copy(&legacy, path));
    match copied {
        Ok(_) => tracing::info!(
            "config PC Hub migrée : {} -> {}",
            legacy.display(),
            path.display()
        ),
        Err(err) => tracing::error!("migration de {} : {err}", legacy.display()),
    }
}

/// Migrer seulement si la nouvelle config n'existe pas et que l'ancienne est un fichier.
fn should_migrate(path: &Path, legacy: &Path) -> bool {
    !path.exists() && legacy.is_file()
}

/// `config.toml` -> `config.toml.<suffix>` dans le même dossier.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(OsString::from).unwrap_or_default();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

/// Charge la config. Absente -> défaut sauvegardé. Illisible ou invalide -> fichier mis de côté
/// en `config.toml.bad` (jamais supprimé) puis défaut.
pub fn load(path: &Path) -> Config {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("pas de config, création de {}", path.display());
            return save_default(path);
        }
        Err(err) => {
            // Fichier présent mais inaccessible : on ne l'écrase pas.
            tracing::error!(
                "lecture {} : {err} ; config par défaut en mémoire",
                path.display()
            );
            return Config::default();
        }
    };
    let parsed = Config::from_toml(&text).and_then(|mut config| {
        config.validate()?;
        Ok(config)
    });
    match parsed {
        Ok(config) => config,
        Err(err) => {
            let backup = with_suffix(path, "bad");
            tracing::error!(
                "{} : {err} ; copie dans {}",
                path.display(),
                backup.display()
            );
            if let Err(err) = std::fs::rename(path, &backup) {
                tracing::error!("sauvegarde de la config invalide impossible : {err}");
                return Config::default(); // ne surtout pas écraser l'original
            }
            save_default(path)
        }
    }
}

fn save_default(path: &Path) -> Config {
    let config = Config::default();
    if let Err(err) = save(path, &config) {
        tracing::error!("{err:#}");
    }
    config
}

/// Écrit `config.toml.tmp` puis le renomme : un crash ne laisse jamais un fichier tronqué.
pub fn save(path: &Path, config: &Config) -> anyhow::Result<()> {
    let text = config.to_toml()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("création de {}", dir.display()))?;
    }
    let tmp = with_suffix(path, "tmp");
    std::fs::write(&tmp, text).with_context(|| format!("écriture de {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("remplacement de {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_path_uses_program_data() {
        assert_eq!(
            windows_path(Some(r"D:\PD".into()), APP_DIR),
            Path::new(r"D:\PD")
                .join("ControlAnything")
                .join("config.toml")
        );
        assert_eq!(
            windows_path(None, LEGACY_DIR),
            Path::new(r"C:\ProgramData")
                .join("PC Hub")
                .join("config.toml")
        );
    }

    #[test]
    fn migrates_only_when_new_missing_and_legacy_present() {
        let dir = std::env::temp_dir().join(format!("ca-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let new = dir.join("new.toml");
        let legacy = dir.join("legacy.toml");
        assert!(!should_migrate(&new, &legacy), "rien à migrer");
        std::fs::write(&legacy, "x").unwrap();
        assert!(should_migrate(&new, &legacy), "ancienne seule -> migrer");
        std::fs::write(&new, "x").unwrap();
        assert!(
            !should_migrate(&new, &legacy),
            "nouvelle déjà là -> ne pas écraser"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn suffix_keeps_directory() {
        let path = Path::new("dir").join("config.toml");
        assert_eq!(
            with_suffix(&path, "bad"),
            Path::new("dir").join("config.toml.bad")
        );
        assert_eq!(
            with_suffix(&path, "tmp"),
            Path::new("dir").join("config.toml.tmp")
        );
    }
}
