//! Control loop: a single thread owns all sensors and controllers.

use crate::config_store;
use crate::providers::{self, Fans, Sensors};
use crate::rgb::RgbCmd;
use hub_core::config::{Config, FanMode, FanSettings, process_key};
use hub_core::control::{Failsafe, FanControl, resolve_source};
use hub_core::fan::{FanChannel, FanStatus};
use hub_core::ipc::{self, ClientMsg, DaemonMsg, Snapshot};
use hub_core::rgb::RgbConfig;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, watch};

/// Messages received by the control thread.
pub enum Command {
    Client(ClientMsg),
    /// The system goes to sleep: fans and lighting handed back to firmware.
    Suspend,
    /// Back from sleep: providers reopened, config applied again.
    Resume,
    Stop,
}

/// Automation rules are evaluated at this pace (process list refresh included).
const AUTOMATION_PERIOD: Duration = Duration::from_secs(2);
/// After `Suspend`, no fan write for this long. Fans stay on firmware control (safe);
/// the limit only matters if the sleep was cancelled and no `Resume` comes.
const SUSPEND_HOLD: Duration = Duration::from_secs(60);

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

/// Channel settings in the automation profile if any, else the active profile; missing = Auto.
fn settings_for(config: &Config, auto: Option<&str>, channel: &str) -> FanSettings {
    auto.and_then(|name| config.profiles.iter().find(|p| p.name == name))
        .or_else(|| config.active())
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
    /// Profile chosen by the automations (`None`: manual `active_profile`).
    auto_profile: Option<String>,
    next_automation: Instant,
    /// Process list, kept only while process rules exist.
    processes: Option<sysinfo::System>,
    /// Set by `Suspend`, cleared by `Resume` or when `SUSPEND_HOLD` runs out.
    suspended_at: Option<Instant>,
    /// Set by `Suspend`, cleared by the first `Resume` (providers reopened once).
    awaiting_resume: bool,
}

/// Per-channel state of each controller, in the same order.
fn channel_states(fans: &Fans) -> Vec<Vec<ChannelState>> {
    fans.iter()
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
        .collect()
}

/// Linux `comm` names are cut to 15 bytes (TASK_COMM_LEN - 1).
const COMM_LEN: Option<usize> = if cfg!(target_os = "linux") {
    Some(15)
} else {
    None
};

/// A rule key matches a running name; with `limit`, only its first `limit` bytes count.
fn process_running(running: &HashSet<String>, key: &str, limit: Option<usize>) -> bool {
    let key = limit.and_then(|n| key.get(..n)).unwrap_or(key);
    running.contains(key)
}

/// While suspended, fans stay on firmware (`None`) unless overheating forces them.
fn suspend_target(suspended: bool, failsafe: bool, target: Option<u8>) -> Option<u8> {
    if suspended && !failsafe { None } else { target }
}

/// Local time of day in minutes, for schedules.
fn local_minute() -> u16 {
    use chrono::Timelike as _;
    let now = chrono::Local::now();
    (now.hour() * 60 + now.minute()) as u16
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
        let states = channel_states(&fans);
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
            auto_profile: None,
            next_automation: Instant::now(),
            processes: None,
            suspended_at: None,
            awaiting_resume: false,
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
                Ok(Command::Suspend) => self.suspend(),
                Ok(Command::Resume) => self.resume(),
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
        self.update_automation();
        if self
            .suspended_at
            .is_some_and(|at| at.elapsed() >= SUSPEND_HOLD)
        {
            tracing::warn!("no resume after suspend: control and lighting taken back");
            self.suspended_at = None;
            let _ = self.rgb.send(RgbCmd::Config(self.config.rgb.clone())); // RGB thread stopped: no effect
        }
        let suspended = self.suspended_at.is_some();
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
                let settings =
                    settings_for(&self.config, self.auto_profile.as_deref(), &channel.id);
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
                // Going to sleep: firmware keeps the fans, except the overheat failsafe.
                let target = suspend_target(suspended, failsafe, target);
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
        let auto = self.auto_profile.clone();
        self.switch(config, auto);
        self.next_automation = Instant::now(); // new rules apply right away
        Ok(())
    }

    /// Applies a config and automation profile; channels whose settings change start from scratch.
    fn switch(&mut self, config: Config, auto: Option<String>) {
        for channel in self.states.iter_mut().flatten() {
            let old = settings_for(&self.config, self.auto_profile.as_deref(), &channel.id);
            if old != settings_for(&config, auto.as_deref(), &channel.id) {
                channel.control = FanControl::new();
            }
        }
        if config.rgb != self.config.rgb {
            let _ = self.rgb.send(RgbCmd::Config(config.rgb.clone())); // RGB thread stopped: no effect
        }
        self.config = config;
        self.auto_profile = auto;
    }

    /// Every `AUTOMATION_PERIOD`: process rule > schedule > manual profile.
    fn update_automation(&mut self) {
        let now = Instant::now();
        if now < self.next_automation {
            return;
        }
        self.next_automation = now + AUTOMATION_PERIOD;
        let running = self.running_processes();
        let auto = self
            .config
            .automation_profile(
                |key| process_running(&running, key, COMM_LEN),
                local_minute(),
            )
            .map(str::to_owned);
        if auto != self.auto_profile {
            let shown = auto.as_deref().unwrap_or(&self.config.active_profile);
            tracing::info!("automation: profile \"{shown}\"");
            self.switch(self.config.clone(), auto);
            self.publish_state();
        }
    }

    /// Process name keys, refreshed only while a process rule exists.
    fn running_processes(&mut self) -> HashSet<String> {
        if self.config.automations.process_rules.is_empty() {
            self.processes = None;
            return HashSet::new();
        }
        if self.processes.is_none() {
            match sysinfo::System::new() {
                Ok(system) => self.processes = Some(system),
                Err(err) => tracing::warn!("process list unavailable: {err}"),
            }
        }
        let Some(system) = self.processes.as_mut() else {
            return HashSet::new();
        };
        system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            sysinfo::ProcessRefreshKind::nothing(),
        );
        system
            .processes()
            .values()
            .map(|p| process_key(&p.name().to_string_lossy()))
            .collect()
    }

    /// Before sleep: fans back to firmware, lighting off. Fan writes pause until `Resume`.
    fn suspend(&mut self) {
        tracing::info!("system suspend: fans to firmware, lighting off");
        for (controller, states) in self.fans.iter_mut().zip(&mut self.states) {
            for channel in states.iter_mut().filter(|c| c.applied.is_some()) {
                if let Err(err) = controller.set_auto(&channel.id) {
                    tracing::warn!("fan {}: {err}", channel.id);
                }
                channel.applied = None;
            }
        }
        let off = RgbConfig {
            brightness: 0,
            ..self.config.rgb.clone()
        };
        let _ = self.rgb.send(RgbCmd::Config(off)); // RGB thread stopped: no effect
        self.suspended_at = Some(Instant::now());
        self.awaiting_resume = true;
    }

    /// After sleep the hardware may have been reset: providers are reopened and
    /// the config applied again from scratch.
    fn resume(&mut self) {
        if !std::mem::take(&mut self.awaiting_resume) {
            return; // Windows sends two resume events: the second one has nothing to do
        }
        self.suspended_at = None;
        tracing::info!("system resume: reopening providers");
        // Old controllers dropped first: their Drop hands the fans back and frees the drivers.
        self.fans.clear();
        self.sensors.clear();
        let (sensors, fans) = providers::build_all();
        self.states = channel_states(&fans);
        self.channels = fans.iter().flat_map(|c| c.channels().to_vec()).collect();
        self.sensor_errors = vec![None; sensors.len()];
        self.sensors = sensors;
        self.fans = fans;
        let _ = self.rgb.send(RgbCmd::Config(self.config.rgb.clone())); // RGB thread stopped: no effect
        self.publish_state();
    }

    fn publish_state(&self) {
        let msg = DaemonMsg::State {
            config: self.config.clone(),
            channels: self.channels.clone(),
            auto_profile: self.auto_profile.clone(),
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
    fn suspend_pauses_writes_but_not_failsafe() {
        assert_eq!(suspend_target(true, false, Some(40)), None);
        assert_eq!(suspend_target(true, true, Some(100)), Some(100));
        assert_eq!(suspend_target(false, false, Some(40)), Some(40));
        // Failsafe over while suspended: a written channel goes back to firmware.
        assert_eq!(
            decide(Some(100), suspend_target(true, false, Some(40))),
            Some(Action::Auto)
        );
    }

    #[test]
    fn process_names_truncated_like_linux_comm() {
        let running: HashSet<String> = ["steamwebhelper".into(), "cyberpunk2077la".into()].into();
        assert!(process_running(&running, "cyberpunk2077launcher", Some(15)));
        assert!(process_running(&running, "steamwebhelper", Some(15)));
        assert!(!process_running(&running, "cyberpunk2077launcher", None));
        assert!(!process_running(&running, "steam", Some(15)));
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
        assert_eq!(settings_for(&config, None, "gpu0/fan0").mode, FanMode::Auto);
        let fixed = FanSettings {
            mode: FanMode::Fixed { duty: 60 },
            ..FanSettings::default()
        };
        config.profiles = vec![
            Profile {
                name: config.active_profile.clone(),
                fans: [("gpu0/fan0".to_owned(), fixed.clone())].into(),
            },
            Profile {
                name: "Night".into(),
                fans: Default::default(),
            },
        ];
        assert_eq!(settings_for(&config, None, "gpu0/fan0"), fixed);
        assert_eq!(settings_for(&config, None, "gpu0/fan1").mode, FanMode::Auto);
        // The automation profile wins; a missing one falls back to the active profile.
        assert_eq!(
            settings_for(&config, Some("Night"), "gpu0/fan0").mode,
            FanMode::Auto
        );
        assert_eq!(settings_for(&config, Some("Gone"), "gpu0/fan0"), fixed);
    }
}
