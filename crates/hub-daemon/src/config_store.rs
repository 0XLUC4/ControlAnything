//! `config.toml` persistence: tolerant loading, atomic saving.

use anyhow::Context;
use hub_core::config::Config;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Application folder under `%ProgramData%`.
const APP_DIR: &str = "ControlAnything";
/// Folder of the former "PC Hub" name, imported once by [`migrate_legacy`].
const LEGACY_DIR: &str = "PC Hub";

/// Location of the daemon system config.
pub fn config_path() -> PathBuf {
    if cfg!(windows) {
        windows_path(std::env::var_os("ProgramData"), APP_DIR)
    } else {
        PathBuf::from("/etc/controlanything/config.toml")
    }
}

/// `%ProgramData%\<dir>\config.toml`, with a fallback if the variable is missing.
fn windows_path(program_data: Option<OsString>, dir: &str) -> PathBuf {
    let base = program_data.map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from);
    base.join(dir).join("config.toml")
}

/// Windows: copies the old config `%ProgramData%\PC Hub\config.toml` if the new one is missing.
/// The old one is left in place (nothing is ever deleted).
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
            "PC Hub config migrated: {} -> {}",
            legacy.display(),
            path.display()
        ),
        Err(err) => tracing::error!("migrating {}: {err}", legacy.display()),
    }
}

/// Migrate only if the new config does not exist and the old one is a file.
fn should_migrate(path: &Path, legacy: &Path) -> bool {
    !path.exists() && legacy.is_file()
}

/// `config.toml` -> `config.toml.<suffix>` in the same folder.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(OsString::from).unwrap_or_default();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

/// Loads the config. Missing -> default saved. Unreadable or invalid -> file set aside
/// as `config.toml.bad` (never deleted), then default.
pub fn load(path: &Path) -> Config {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("no config, creating {}", path.display());
            return save_default(path);
        }
        Err(err) => {
            // File present but inaccessible: do not overwrite it.
            tracing::error!(
                "reading {}: {err}; default config in memory",
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
                "{}: {err}; copied to {}",
                path.display(),
                backup.display()
            );
            if let Err(err) = std::fs::rename(path, &backup) {
                tracing::error!("cannot back up the invalid config: {err}");
                return Config::default(); // never overwrite the original
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

/// Writes `config.toml.tmp` then renames it: a crash never leaves a truncated file.
pub fn save(path: &Path, config: &Config) -> anyhow::Result<()> {
    let text = config.to_toml()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = with_suffix(path, "tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
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
        assert!(!should_migrate(&new, &legacy), "nothing to migrate");
        std::fs::write(&legacy, "x").unwrap();
        assert!(should_migrate(&new, &legacy), "old only -> migrate");
        std::fs::write(&new, "x").unwrap();
        assert!(
            !should_migrate(&new, &legacy),
            "new already there -> do not overwrite"
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
