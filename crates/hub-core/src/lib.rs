//! ControlAnything core: sensor types, hardware backend traits, IPC protocol.
//! No UI or OS dependency: shared by the daemon, the backends and the UI.

use serde::{Deserialize, Serialize};

/// Kind of a reading, which determines its unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SensorKind {
    Temperature,
    Load,
    Fan,
    Power,
    Clock,
    Memory,
}

impl SensorKind {
    /// Display unit of the raw value.
    pub fn unit(self) -> &'static str {
        match self {
            Self::Temperature => "°C",
            Self::Load => "%",
            Self::Fan => "RPM",
            Self::Power => "W",
            Self::Clock => "MHz",
            Self::Memory => "MiB",
        }
    }
}

/// Hardware component a sensor belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Device {
    Cpu,
    Gpu,
    Motherboard,
    Memory,
    Storage,
}

/// An instantaneous reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    /// Stable unique id, e.g. `cpu/package_temp`. Used as the history key.
    pub id: String,
    pub label: String,
    pub device: Device,
    pub kind: SensorKind,
    pub value: f32,
}

/// Errors reported by the hardware backends.
#[derive(Debug, thiserror::Error)]
pub enum HwError {
    #[error("access denied (admin/root rights required): {0}")]
    PermissionDenied(String),
    #[error("hardware or driver unavailable: {0}")]
    Unavailable(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

/// Sensor source (one backend per hardware family).
pub trait SensorProvider: Send {
    /// Short name for the logs.
    fn name(&self) -> &str;
    /// Appends the current readings to `out`.
    fn poll(&mut self, out: &mut Vec<Reading>) -> Result<(), HwError>;
}

pub mod config;
pub mod control;
pub mod fan;
pub mod rgb;

/// Daemon <-> UI protocol: one JSON line per message.
pub mod ipc {
    use super::Reading;
    use crate::config::Config;
    use crate::fan::{FanChannel, FanStatus};
    use crate::rgb::RgbDeviceInfo;
    use serde::de::DeserializeOwned;
    use serde::{Deserialize, Serialize};

    /// Messages sent by the daemon.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum DaemonMsg {
        /// Readings, every tick.
        Snapshot(Snapshot),
        /// Applied targets + failsafe state, every tick.
        Fans {
            fans: Vec<FanStatus>,
            failsafe: bool,
        },
        /// Current config and detected channels: on connection and after each change.
        State {
            config: Config,
            channels: Vec<FanChannel>,
            /// Profile forced by an automation rule (`None`: manual profile).
            #[serde(default)]
            auto_profile: Option<String>,
        },
        /// Detected RGB devices: on connection and when the list changes.
        Rgb { devices: Vec<RgbDeviceInfo> },
        /// Rejected command (invalid config, unknown profile, etc.).
        Error { message: String },
    }

    /// Commands sent by the UI. The daemon re-validates everything: it never trusts the client.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum ClientMsg {
        /// Replaces the whole config (validated then saved by the daemon).
        SetConfig { config: Config },
        /// Switches the active profile.
        SetProfile { name: String },
    }

    /// Name of the Windows named pipe exposed by the daemon.
    pub const PIPE_NAME: &str = r"\\.\pipe\controlanything";
    /// Unix socket exposed by the daemon (Linux).
    pub const SOCKET_PATH: &str = "/run/controlanything/daemon.sock";

    /// Full sensor state at a given instant.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Snapshot {
        /// Milliseconds since the Unix epoch.
        pub timestamp_ms: u64,
        pub readings: Vec<Reading>,
    }

    /// Serializes a message into one line ending with `\n`.
    pub fn encode<T: Serialize>(msg: &T) -> serde_json::Result<String> {
        let mut line = serde_json::to_string(msg)?;
        line.push('\n');
        Ok(line)
    }

    /// Deserializes a received line.
    pub fn decode<T: DeserializeOwned>(line: &str) -> serde_json::Result<T> {
        serde_json::from_str(line.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_roundtrip() {
        let snapshot = ipc::Snapshot {
            timestamp_ms: 42,
            readings: vec![Reading {
                id: "cpu/package_temp".into(),
                label: "CPU Package".into(),
                device: Device::Cpu,
                kind: SensorKind::Temperature,
                value: 51.5,
            }],
        };
        let msg = ipc::DaemonMsg::Snapshot(snapshot);
        let line = ipc::encode(&msg).expect("encode");
        assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
        assert_eq!(ipc::decode::<ipc::DaemonMsg>(&line).expect("decode"), msg);
    }
}
