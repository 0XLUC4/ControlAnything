//! Instance unique de l'UI : une seconde instance doit quitter aussitôt.
//! En cas d'erreur système, on laisse démarrer : mieux vaut une UI de trop qu'aucune UI.

/// Verrou à garder vivant toute la durée du programme ; `None` si une autre instance tourne.
#[cfg(windows)]
pub fn acquire() -> Option<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::core::w;

    // SAFETY : nom constant terminé par un nul, attributs de sécurité par défaut.
    match unsafe { CreateMutexW(None, false, w!("Local\\ControlAnything.UI")) } {
        // SAFETY : lu juste après CreateMutexW, sans appel système intermédiaire.
        // Le handle n'est jamais fermé : Windows le libère à la fin du processus.
        Ok(handle) => (unsafe { GetLastError() } != ERROR_ALREADY_EXISTS).then_some(handle),
        Err(err) => {
            tracing::warn!("mutex d'instance unique indisponible : {err}");
            Some(HANDLE::default())
        }
    }
}

/// Verrou à garder vivant toute la durée du programme ; `None` si une autre instance tourne.
#[cfg(unix)]
pub fn acquire() -> Option<Option<std::fs::File>> {
    let dir =
        std::env::var_os("XDG_RUNTIME_DIR").map_or_else(|| "/tmp".into(), std::path::PathBuf::from);
    match lock_file(&dir.join("controlanything-ui.lock")) {
        Ok(lock) => lock.map(Some),
        Err(err) => {
            tracing::warn!("verrou d'instance unique indisponible : {err}");
            Some(None)
        }
    }
}

/// Verrou exclusif non bloquant ; `Ok(None)` si le fichier est déjà verrouillé.
#[cfg(any(unix, test))]
fn lock_file(path: &std::path::Path) -> std::io::Result<Option<std::fs::File>> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(err)) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn second_lock_fails_until_first_released() {
        let path =
            std::env::temp_dir().join(format!("controlanything-test-{}.lock", std::process::id()));
        let first = super::lock_file(&path).ok().flatten();
        assert!(first.is_some());
        assert!(super::lock_file(&path).ok().flatten().is_none());
        drop(first);
        assert!(super::lock_file(&path).ok().flatten().is_some());
        let _ = std::fs::remove_file(&path);
    }
}
