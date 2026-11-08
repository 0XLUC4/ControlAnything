//! Single UI instance: a second instance must exit immediately.
//! On a system error, startup proceeds: one UI too many beats no UI at all.

/// Lock to keep alive for the whole program; `None` if another instance is running.
#[cfg(windows)]
pub fn acquire() -> Option<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::core::w;

    // SAFETY: constant nul-terminated name, default security attributes.
    match unsafe { CreateMutexW(None, false, w!("Local\\ControlAnything.UI")) } {
        // SAFETY: read right after CreateMutexW, with no system call in between.
        // The handle is never closed: Windows releases it when the process exits.
        Ok(handle) => (unsafe { GetLastError() } != ERROR_ALREADY_EXISTS).then_some(handle),
        Err(err) => {
            tracing::warn!("single-instance mutex unavailable: {err}");
            Some(HANDLE::default())
        }
    }
}

/// Lock to keep alive for the whole program; `None` if another instance is running.
#[cfg(unix)]
pub fn acquire() -> Option<Option<std::fs::File>> {
    let dir =
        std::env::var_os("XDG_RUNTIME_DIR").map_or_else(|| "/tmp".into(), std::path::PathBuf::from);
    match lock_file(&dir.join("controlanything-ui.lock")) {
        Ok(lock) => lock.map(Some),
        Err(err) => {
            tracing::warn!("single-instance lock unavailable: {err}");
            Some(None)
        }
    }
}

/// Non-blocking exclusive lock; `Ok(None)` if the file is already locked.
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
