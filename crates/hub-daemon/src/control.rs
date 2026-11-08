//! Control loop: a single thread owns all sensors and controllers.

use crate::config_store;
use crate::providers::{Fans, Sensors};
use crate::rgb::RgbCmd;
use hub_core::config::{Config, FanMode, FanSettings};
use hub_core::control::{Failsafe, FanControl, resolve_source};
use hub_core::fan::{FanChannel, FanStatus};
use hub_core::ipc::{self, ClientMsg, DaemonMsg, Snapshot};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, watch};

/// Messages received by the control thread.
pub enum Command {
    Client(ClientMsg),
    Stop,
}

/// Hardware write to perform on a channel.
#[derive(Debug, PartialEq)]
enum Action {
    Duty(u8),
    Auto,
}

/// Writes only on change: `applied` = last successful target (`None` = firmware).
fn decide(applied: Option<u8>, target: Option<u8>) -> Option<Action> {
    match (applied, target) {
        (_, Some(duty)) if applied != Some(duty) => Some(Action::Duty(duty)),
        (Some(_), None) => Some(Action::Auto),
        _ => None,
    }
}

/// Floor enforced on AIO pumps: a stopped pump overheats the CPU within seconds.
const PUMP_MIN_DUTY: u8 = 60;

/// A pump channel is recognized by its name ("Pump Fan" on MSI, `pump` elsewhere).
fn is_pump(channel: &FanChannel) -> bool {
    channel.label.to_lowercase().contains("pump") || channel.id.to_lowercase().contains("pump")
}

/// Channel settings in the active profile; missing = Auto.
fn settings_for(config: &Config, channel: &str) -> FanSettings {
    config
        .active()
        .and_then(|profile| profile.fans.get(channel))
        .cloned()
        .unwrap_or_default()
}

struct ChannelState {
    id: String,
    pump: bool,
    control: FanControl,
    applied: Option<u8>,
    last_error: Option<String>,
}

pub struct Hub {
    path: PathBuf,
    config: Config,
    sensors: Sensors,
    sensor_errors: Vec<Option<String>>,
    /// Dropped at the end of `run`: their `Drop` hands the fans back to the BIOS.
    fans: Fans,
    states: Vec<Vec<ChannelState>>,
    channels: Vec<FanChannel>,
    failsafe: Failsafe,
    overheating: bool,
    live: broadcast::Sender<String>,
    state: watch::Sender<String>,
    /// RGB thread: receives readings and RGB config (non-blocking send, timing unchanged).
    rgb: Sender<RgbCmd>,
}

impl Hub {
    pub fn new(
        path: PathBuf,
        config: Config,
        sensors: Sensors,
        fans: Fans,
        live: broadcast::Sender<String>,
        state: watch::Sender<String>,
        rgb: Sender<RgbCmd>,
    ) -> Self {
        let states = fans
            .iter()
            .map(|controller| {
                controller
                    .channels()
                    .iter()
                    .map(|channel| ChannelState {
                        id: channel.id.clone(),
                        pump: is_pump(channel),
                        control: FanControl::new(),
                        applied: None,
                        last_error: None,
                    })
                    .collect()
            })
            .collect();
        let channels = fans.iter().flat_map(|c| c.channels().to_vec()).collect();
        let hub = Self {
            path,
            config,
            sensor_errors: vec![None; sensors.len()],
            sensors,
            fans,
            states,
            channels,
            failsafe: Failsafe::default(),
            overheating: false,
            live,
            state,
            rgb,
        };
        hub.publish_state();
        hub
    }

    /// Runs until `Stop` (or channel closure). Commands wake the loop immediately.
    pub fn run(mut self, commands: Receiver<Command>) {
        tracing::info!(
            "{} sensor(s), {} fan channel(s)",
            self.sensors.len(),
            self.channels.len()
        );
        let mut next_tick = Instant::now();
        loop {
            match commands.recv_timeout(next_tick.saturating_duration_since(Instant::now())) {
                Ok(Command::Client(msg)) => {
                    if !self.handle(msg) {
                        continue; // nothing changed: keep the normal pace
                    }
                }
                Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.tick();
            next_tick = Instant::now() + Duration::from_millis(self.config.poll_interval_ms);
        }
        tracing::info!("stopping: fans handed back to firmware");
        // `self` dropped here -> controllers Drop -> restore_all.
    }

    fn tick(&mut self) {
        let mut readings = Vec::new();
        for (provider, last_error) in self.sensors.iter_mut().zip(&mut self.sensor_errors) {
            let error = provider.poll(&mut readings).err().map(|e| e.to_string());
            if let Some(message) = &error
                && error != *last_error
            {
                tracing::warn!("poll {}: {message}", provider.name());
            }
            *last_error = error;
        }

        let failsafe = self.failsafe.update(&self.config.failsafe, &readings);
        if failsafe && !self.overheating {
            tracing::error!("OVERHEATING: all controlled fans go to 100%");
        } else if !failsafe && self.overheating {
            tracing::warn!("overheating over, back to profile targets");
        }
        self.overheating = failsafe;

        let now = Instant::now();
        let mut statuses = Vec::with_capacity(self.channels.len());
        for (controller, states) in self.fans.iter_mut().zip(&mut self.states) {
            for channel in states {
                let settings = settings_for(&self.config, &channel.id);
                let temp = match &settings.mode {
                    FanMode::Curve { source, .. } => resolve_source(source, &readings),
                    _ => None,
                };
                let mut target = channel.control.update(&settings, temp, now);
                if failsafe && settings.mode != FanMode::Auto {
                    target = Some(100);
                }
                if channel.pump {
                    // No zero RPM and no low target on a pump, whatever the config.
                    target = target.map(|duty| duty.max(PUMP_MIN_DUTY));
                }
                if let Some(action) = decide(channel.applied, target) {
                    let result = match action {
                        Action::Duty(duty) => controller.set_duty(&channel.id, duty),
                        Action::Auto => controller.set_auto(&channel.id),
                    };
                    match result {
                        Ok(()) => {
                            channel.applied = target;
                            channel.last_error = None;
                        }
                        Err(err) => {
                            let message = err.to_string();
                            if channel.last_error.as_ref() != Some(&message) {
                                tracing::warn!("fan {}: {message}", channel.id);
                            }
                            channel.last_error = Some(message);
                        }
                    }
                }
                statuses.push(FanStatus {
                    channel: channel.id.clone(),
                    duty: channel.applied,
                });
            }
        }

        let _ = self.rgb.send(RgbCmd::Readings(readings.clone())); // RGB thread stopped: no effect
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        self.publish(&DaemonMsg::Snapshot(Snapshot {
            timestamp_ms,
            readings,
        }));
        self.publish(&DaemonMsg::Fans {
            fans: statuses,
            failsafe,
        });
    }

    /// Handles a client command; true if the config changed.
    fn handle(&mut self, msg: ClientMsg) -> bool {
        let result = match msg {
            ClientMsg::SetConfig { config } => self.set_config(config),
            ClientMsg::SetProfile { name } => {
                if self.config.profiles.iter().any(|p| p.name == name) {
                    let mut config = self.config.clone();
                    config.active_profile = name;
                    self.set_config(config)
                } else {
                    Err(format!("unknown profile: {name}"))
                }
            }
        };
        match result {
            Ok(()) => {
                self.publish_state();
                true
            }
            Err(message) => {
                tracing::warn!("command rejected: {message}");
                self.publish(&DaemonMsg::Error { message });
                false
            }
        }
    }

    /// Validates, saves then applies; channels whose settings change start from scratch.
    fn set_config(&mut self, mut config: Config) -> Result<(), String> {
        config.validate().map_err(|e| e.to_string())?;
        config_store::save(&self.path, &config).map_err(|e| format!("{e:#}"))?;
        for channel in self.states.iter_mut().flatten() {
            if settings_for(&self.config, &channel.id) != settings_for(&config, &channel.id) {
                channel.control = FanControl::new();
            }
        }
        if config.rgb != self.config.rgb {
            let _ = self.rgb.send(RgbCmd::Config(config.rgb.clone())); // RGB thread stopped: no effect
        }
        self.config = config;
        Ok(())
    }

    fn publish_state(&self) {
        let msg = DaemonMsg::State {
            config: self.config.clone(),
            channels: self.channels.clone(),
        };
        match ipc::encode(&msg) {
            Ok(line) => {
                self.state.send_replace(line.clone());
                let _ = self.live.send(line); // no client: nothing to do
            }
            Err(err) => tracing::error!("encoding state: {err}"),
        }
    }

    fn publish(&self, msg: &DaemonMsg) {
        match ipc::encode(msg) {
            Ok(line) => {
                let _ = self.live.send(line); // no client: nothing to do
            }
            Err(err) => tracing::error!("encoding message: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_core::config::Profile;

    #[test]
    fn pump_detected_by_label_or_id() {
        let channel = |id: &str, label: &str| FanChannel {
            id: id.into(),
            label: label.into(),
            rpm_sensor: None,
        };
        assert!(is_pump(&channel("nct6687/pwm2", "Pump Fan")));
        assert!(is_pump(&channel("aio/pump", "AIO")));
        assert!(!is_pump(&channel("nct6687/pwm3", "System Fan 1")));
    }

    #[test]
    fn decide_writes_only_on_change() {
        assert_eq!(decide(None, None), None);
        assert_eq!(decide(None, Some(40)), Some(Action::Duty(40)));
        assert_eq!(decide(Some(40), Some(40)), None);
        assert_eq!(decide(Some(40), Some(55)), Some(Action::Duty(55)));
        assert_eq!(decide(Some(40), None), Some(Action::Auto));
    }

    #[test]
    fn missing_channel_defaults_to_auto() {
        let mut config = Config::default();
        assert_eq!(settings_for(&config, "gpu0/fan0").mode, FanMode::Auto);
        let fixed = FanSettings {
            mode: FanMode::Fixed { duty: 60 },
            ..FanSettings::default()
        };
        config.profiles = vec![Profile {
            name: config.active_profile.clone(),
            fans: [("gpu0/fan0".to_owned(), fixed.clone())].into(),
        }];
        assert_eq!(settings_for(&config, "gpu0/fan0"), fixed);
        assert_eq!(settings_for(&config, "gpu0/fan1").mode, FanMode::Auto);
    }
}
