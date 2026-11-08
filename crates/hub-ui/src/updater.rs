//! Self-update from the GitHub releases (Windows): check, download, run the installer.
//!
//! HTTP goes through `curl.exe` (shipped with Windows 10+): no TLS stack in the app.
//! The NSIS installer stops the service and the running app, then restarts both (`/P /R`).
//! An installer is only run when its size and the SHA-256 published by GitHub match.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use gpui_kit::Context;

use crate::app::HubApp;

const LATEST_URL: &str = "https://api.github.com/repos/0XLUC4/ControlAnything/releases/latest";
/// Only installers from this repository are downloaded.
const DOWNLOAD_PREFIX: &str = "https://github.com/0XLUC4/ControlAnything/releases/download/";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// Download folders are `%TEMP%\controlanything-update-<random>`.
const DIR_PREFIX: &str = "controlanything-update-";

/// A release newer than the running version.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    url: String,
    size: u64,
    /// Lowercase hex SHA-256 from the GitHub asset `digest`.
    sha256: String,
    /// Release notes (GitHub `body`), shortened for the popup.
    pub notes: String,
}

/// What the update checks found; shared with the background threads.
#[derive(Default)]
pub struct UpdateState {
    newer: Arc<Mutex<Option<Release>>>,
    busy: Arc<AtomicBool>,
    /// Bytes of the installer written so far (download progress bar).
    downloaded: Arc<AtomicU64>,
    /// "Update and restart" was clicked and the download or install runs.
    downloading: bool,
    /// Result of the last action, shown in the update popup.
    message: Option<String>,
}

impl UpdateState {
    /// Starts a background check (Windows); the title bar refresh picks up the result.
    pub fn start() -> Self {
        let state = Self::default();
        if cfg!(windows) {
            let (newer, busy) = (state.newer.clone(), state.busy.clone());
            busy.store(true, Ordering::Relaxed);
            std::thread::spawn(move || {
                match fetch_latest() {
                    Ok(found) => *lock(&newer) = found,
                    Err(err) => tracing::warn!("update check: {err}"),
                }
                busy.store(false, Ordering::Relaxed);
            });
        }
        state
    }

    fn newer(&self) -> Option<Release> {
        lock(&self.newer).clone()
    }
}

fn lock(newer: &Mutex<Option<Release>>) -> std::sync::MutexGuard<'_, Option<Release>> {
    newer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A newer version is known: the title bar button is highlighted.
pub fn has_update(state: &UpdateState) -> bool {
    lock(&state.newer).is_some()
}

/// The newer release, if any (version and notes for the popup).
pub fn release(state: &UpdateState) -> Option<Release> {
    state.newer()
}

/// Download progress in percent, `None` when no download runs.
pub fn download_progress(state: &UpdateState) -> Option<f32> {
    let size = state.newer()?.size;
    state
        .downloading
        .then(|| state.downloaded.load(Ordering::Relaxed) as f32 * 100. / size as f32)
}

/// A check or download runs: the buttons show a spinner and ignore clicks.
pub fn busy(state: &UpdateState) -> bool {
    state.busy.load(Ordering::Relaxed)
}

/// Progress or result line for the update popup (download, error, cancel, checking).
pub fn status(state: &UpdateState) -> Option<String> {
    if let Some(message) = &state.message {
        return Some(message.clone());
    }
    busy(state).then(|| "Checking for updates...".into())
}

/// "Check again" in the update popup.
pub fn check(app: &mut HubApp, cx: &mut Context<HubApp>) {
    if !cfg!(windows) {
        app.update.message = Some("Updates via your package manager.".into());
        cx.notify();
        return;
    }
    if busy(&app.update) {
        return;
    }
    app.update.message = None;
    run_busy(app, cx, fetch_latest, |app, result, _| match result {
        Ok(found) => *lock(&app.update.newer) = found,
        Err(err) => app.update.message = Some(format!("Check failed: {err}")),
    });
    cx.notify();
}

/// "Install" in the update popup: download, verify, run the installer.
pub fn install_latest(app: &mut HubApp, cx: &mut Context<HubApp>) {
    let Some(release) = app.update.newer() else {
        return;
    };
    if busy(&app.update) {
        return;
    }
    app.update.message = None;
    app.update.downloading = true;
    let downloaded = app.update.downloaded.clone();
    downloaded.store(0, Ordering::Relaxed);
    run_busy(
        app,
        cx,
        move || install(&release, &downloaded),
        |app, result, cx| {
            app.update.downloading = false;
            match result {
                // The installer closes the app anyway; quitting now avoids its prompt.
                Ok(true) => cx.quit(),
                Ok(false) => app.update.message = Some("Update cancelled.".into()),
                Err(err) => app.update.message = Some(err),
            }
        },
    );
    cx.notify();
}

/// Runs `work` off the UI thread with the button busy, then `done` on the UI thread.
fn run_busy<T: Send + 'static>(
    app: &mut HubApp,
    cx: &mut Context<HubApp>,
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(&mut HubApp, T, &mut Context<HubApp>) + 'static,
) {
    let busy = app.update.busy.clone();
    busy.store(true, Ordering::Relaxed);
    cx.spawn(async move |this, cx| {
        let result = cx.background_executor().spawn(async move { work() }).await;
        busy.store(false, Ordering::Relaxed);
        let _ = this.update(cx, |app, cx| {
            done(app, result, cx);
            cx.notify();
        });
    })
    .detach();
}

/// Parses "v1.2.3" / "1.2.3-beta" into (1, 2, 3); missing parts are 0.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let core = text
        .trim()
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let major = parts.next()??;
    let minor = parts.next().unwrap_or(Some(0))?;
    let patch = parts.next().unwrap_or(Some(0))?;
    Some((major, minor, patch))
}

/// Latest GitHub release if newer than this build, with its x64 installer.
fn fetch_latest() -> Result<Option<Release>, String> {
    let body = curl(&["-H", "Accept: application/vnd.github+json", LATEST_URL])?;
    parse_release(&String::from_utf8_lossy(&body), CURRENT)
}

fn parse_release(json: &str, current: &str) -> Result<Option<Release>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|err| format!("unexpected answer: {err}"))?;
    let tag = value["tag_name"].as_str().ok_or("release without a tag")?;
    let (Some(latest), Some(running)) = (parse_version(tag), parse_version(current)) else {
        return Ok(None);
    };
    if latest <= running {
        return Ok(None);
    }
    // Rebuilt from numbers: the raw tag never reaches a path or a command.
    let version = format!("{}.{}.{}", latest.0, latest.1, latest.2);
    let name = format!("controlanything_{version}_x64-setup.exe");
    let asset = value["assets"]
        .as_array()
        .and_then(|assets| assets.iter().find(|a| a["name"] == name.as_str()))
        .ok_or_else(|| format!("{name} not found in release v{version}"))?;
    let url = asset["browser_download_url"].as_str().unwrap_or_default();
    if !url.starts_with(DOWNLOAD_PREFIX) {
        return Err(format!("unexpected download address: {url}"));
    }
    let sha256 = asset["digest"]
        .as_str()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .map(str::to_lowercase)
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("the release has no SHA-256 checksum, update refused")?;
    let size = asset["size"]
        .as_u64()
        .filter(|&size| size > 0)
        .ok_or("the release does not give the installer size, update refused")?;
    Ok(Some(Release {
        version,
        url: url.to_owned(),
        size,
        sha256,
        notes: shorten(value["body"].as_str().unwrap_or_default(), NOTES_MAX),
    }))
}

/// Release notes longer than this are cut (the popup stays small).
const NOTES_MAX: usize = 600;

/// First `max` chars of `text` (trimmed), with "..." when cut.
fn shorten(text: &str, max: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}...", text[..end].trim_end()),
        None => text.to_owned(),
    }
}

/// Downloads the installer into a fresh folder, checks it, then starts it.
/// `Ok(false)`: the user declined the UAC prompt.
fn install(release: &Release, downloaded: &AtomicU64) -> Result<bool, String> {
    remove_stale_dirs();
    let dir = std::env::temp_dir().join(format!("{DIR_PREFIX}{:016x}", random_u64()));
    // create_dir (not create_dir_all) fails if the folder already exists.
    std::fs::create_dir(&dir).map_err(|err| format!("cannot create the download folder: {err}"))?;
    let path = dir.join("setup.exe");
    let path_text = path.to_string_lossy().into_owned();
    let finished = AtomicBool::new(false);
    // A side thread reports the file size while curl writes it (progress bar).
    let fetched = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !finished.load(Ordering::Relaxed) {
                let size = std::fs::metadata(&path).map_or(0, |m| m.len());
                downloaded.store(size, Ordering::Relaxed);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        });
        let fetched = curl(&["--max-time", "600", "-o", &path_text, &release.url]);
        finished.store(true, Ordering::Relaxed);
        fetched
    });
    let result = fetched
        .and_then(|_| verify(release, &path_text))
        .and_then(|()| launch(&path_text));
    if !matches!(result, Ok(true)) {
        let _ = std::fs::remove_dir_all(&dir);
    }
    result
}

/// Size and SHA-256 must match the release; checked right before launching.
fn verify(release: &Release, path: &str) -> Result<(), String> {
    let size = std::fs::metadata(path).map_or(0, |m| m.len());
    if size != release.size {
        return Err(format!("download incomplete ({size} bytes)"));
    }
    if sha256_of(path)? != release.sha256 {
        return Err("downloaded installer does not match its checksum".into());
    }
    Ok(())
}

/// Best effort: folders left by earlier updates (a running installer may still hold one).
fn remove_stale_dirs() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(DIR_PREFIX) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Unpredictable folder name: std's randomly seeded hasher mixed with time and pid.
fn random_u64() -> u64 {
    use std::hash::{BuildHasher as _, Hasher as _};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    hasher.write_u128(nanos);
    hasher.write_u32(std::process::id());
    hasher.finish()
}

/// ShellExecuteW "runas" shows the UAC prompt the installer needs.
/// /P: passive install, /R: restart the app. `Ok(false)`: the user cancelled UAC.
#[cfg(windows)]
fn launch(path: &str) -> Result<bool, String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR, w};
    const ERROR_CANCELLED: i32 = 1223;
    let file = HSTRING::from(path);
    // SAFETY: every string is a valid NUL-terminated UTF-16 buffer alive during the call.
    let code = unsafe {
        ShellExecuteW(
            None,
            w!("runas"),
            PCWSTR(file.as_ptr()),
            w!("/P /R"),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    }
    .0 as isize;
    if code > 32 {
        return Ok(true);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(ERROR_CANCELLED) => Ok(false),
        _ => Err(format!("cannot start the installer (code {code})")),
    }
}

#[cfg(not(windows))]
fn launch(_path: &str) -> Result<bool, String> {
    Err("Windows only".into())
}

/// Absolute path of a tool in %SystemRoot%\System32, so PATH cannot substitute it.
fn system32(program: &str) -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    std::path::Path::new(&root).join("System32").join(program)
}

/// SHA-256 in lowercase hex via `certutil` (shipped with Windows).
fn sha256_of(path: &str) -> Result<String, String> {
    let output = hidden(&system32("certutil.exe"))
        .args(["-hashfile", path, "SHA256"])
        .output()
        .map_err(|err| format!("certutil: {err}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    // Line 2 holds the hash (with spaces on old Windows versions).
    text.lines()
        .nth(1)
        .map(|line| line.replace(' ', "").to_lowercase())
        .filter(|hash| hash.len() == 64)
        .ok_or_else(|| "certutil could not hash the installer".into())
}

/// `curl.exe` with redirects (HTTPS only), failure on HTTP errors and the User-Agent GitHub requires.
fn curl(args: &[&str]) -> Result<Vec<u8>, String> {
    let output = hidden(&system32("curl.exe"))
        .args([
            "-fsSL",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--connect-timeout",
            "10",
            "-A",
            "ControlAnything-updater",
        ])
        .args(args)
        .output()
        .map_err(|err| format!("curl unavailable: {err}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(format!(
            "network error: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Console program started without flashing a console window.
fn hidden(program: &std::path::Path) -> std::process::Command {
    #[allow(unused_mut)]
    let mut command = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_newer(candidate: &str, current: &str) -> bool {
        matches!((parse_version(candidate), parse_version(current)), (Some(a), Some(b)) if a > b)
    }

    #[test]
    fn version_comparison() {
        assert!(is_newer("v0.4.0", "0.3.0"));
        assert!(is_newer("0.3.10", "0.3.9"));
        assert!(is_newer("v1.0", "0.9.9"));
        assert!(!is_newer("v0.3.0", "0.3.0"));
        assert!(!is_newer("v0.2.9", "0.3.0"));
        assert!(!is_newer("v0.4.0-beta", "0.4.0"));
        assert!(!is_newer("nightly", "0.3.0"));
    }

    #[test]
    fn release_parsing() {
        let json = r#"{"tag_name":"v0.4.0","assets":[
            {"name":"other.zip","browser_download_url":"https://github.com/0XLUC4/ControlAnything/releases/download/v0.4.0/other.zip","size":1},
            {"name":"controlanything_0.4.0_x64-setup.exe","size":42,"digest":"sha256:ABCD000000000000000000000000000000000000000000000000000000000000",
             "browser_download_url":"https://github.com/0XLUC4/ControlAnything/releases/download/v0.4.0/controlanything_0.4.0_x64-setup.exe"}]}"#;
        let release = parse_release(json, "0.3.0").unwrap().unwrap();
        assert_eq!(release.version, "0.4.0");
        assert_eq!(release.size, 42);
        assert_eq!(release.sha256, format!("abcd{}", "0".repeat(60)));
        assert_eq!(parse_release(json, "0.4.0"), Ok(None));
        let foreign = json.replace("github.com/0XLUC4", "evil.example");
        assert!(parse_release(&foreign, "0.3.0").is_err());
        assert!(parse_release(r#"{"tag_name":"v0.4.0","assets":[]}"#, "0.3.0").is_err());
        // Checksum and size are mandatory.
        let no_digest = json.replace("\"digest\"", "\"other\"");
        assert!(parse_release(&no_digest, "0.3.0").is_err());
        let bad_digest = json.replace("sha256:ABCD0", "sha256:ABCD");
        assert!(parse_release(&bad_digest, "0.3.0").is_err());
        let no_size = json.replace("\"size\":42", "\"size\":0");
        assert!(parse_release(&no_size, "0.3.0").is_err());
    }

    #[test]
    fn version_rebuilt_from_numbers() {
        let json = r#"{"tag_name":"v1.0","assets":[
            {"name":"controlanything_1.0.0_x64-setup.exe","size":1,
             "digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000",
             "browser_download_url":"https://github.com/0XLUC4/ControlAnything/releases/download/v1.0/controlanything_1.0.0_x64-setup.exe"}]}"#;
        let release = parse_release(json, "0.3.0").unwrap().unwrap();
        assert_eq!(release.version, "1.0.0");
    }

    #[test]
    fn notes_are_shortened() {
        assert_eq!(shorten("  short  ", 10), "short");
        assert_eq!(shorten("abcdef", 3), "abc...");
        assert_eq!(shorten("éééé", 2), "éé...");
    }

    #[test]
    fn random_names_differ() {
        assert_ne!(random_u64(), random_u64());
    }
}
