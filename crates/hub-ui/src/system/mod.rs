//! Per-user system backends (audio endpoints, network adapters).
//! They run in the UI process: it is the logged-in user, unlike the service.

pub mod audio;
pub mod net;

/// State of the System pages, owned by `HubApp`.
#[derive(Default)]
pub struct SystemPages {
    pub audio: crate::audio::AudioPage,
    pub network: crate::network::NetworkPage,
}

/// Error of every backend outside Windows.
#[cfg(not(windows))]
pub const UNAVAILABLE: &str = "Not available on Linux yet.";
