//! Shared UI state: last daemon message, config draft and pure editing rules.

use std::time::{Duration, Instant};

use hub_core::config::{Config, FanMode, FanSettings, Profile, TempSource};
use hub_core::fan::{FanChannel, FanStatus};
use hub_core::ipc::{DaemonMsg, Snapshot};
use hub_core::rgb::RgbDeviceInfo;
use hub_core::{Device, Reading, SensorKind};

/// Beyond this, the daemon is considered disconnected.
const STALE_AFTER: Duration = Duration::from_secs(3);
/// A sent config without answer stops showing "pending" after this delay.
const PENDING_TIMEOUT: Duration = Duration::from_secs(5);
/// GPU temperature id (NVIDIA backend).
pub const GPU_TEMP: &str = "gpu0/temp";

/// Message shown at the top of the page.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    Info(String),
    Error(String),
}

#[derive(Default)]
pub struct HubState {
    pub latest: Option<Snapshot>,
    last_update: Option<Instant>,
    /// Config confirmed by the last daemon `State`.
    pub config: Option<Config>,
    /// Local copy edited by the UI, sent by "Apply".
    pub draft: Option<Config>,
    pub channels: Vec<FanChannel>,
    pub fans: Vec<FanStatus>,
    pub failsafe: bool,
    /// RGB devices detected by the daemon.
    pub rgb_devices: Vec<RgbDeviceInfo>,
    pub notice: Option<Notice>,
    /// A `SetConfig` awaits its answer: the next `State` replaces the draft.
    pub pending_apply: bool,
    /// When the pending `SetConfig` was sent (see [`HubState::start_pending`]).
    pending_since: Option<Instant>,
}

impl HubState {
    /// Applies a daemon message.
    pub fn receive(&mut self, msg: DaemonMsg) {
        match msg {
            DaemonMsg::Snapshot(snapshot) => {
                self.latest = Some(snapshot);
                self.last_update = Some(Instant::now());
            }
            DaemonMsg::Fans { fans, failsafe } => {
                self.fans = fans;
                self.failsafe = failsafe;
            }
            DaemonMsg::State {
                config, channels, ..
            } => self.on_state(config, channels),
            DaemonMsg::Rgb { devices } => self.rgb_devices = devices,
            DaemonMsg::Error { message } => {
                self.pending_apply = false;
                self.pending_since = None;
                self.notice = Some(Notice::Error(message));
            }
        }
    }

    fn on_state(&mut self, config: Config, channels: Vec<FanChannel>) {
        // Unsent local edits survive a profile switch; a timed out apply counts as unsent.
        let keep_edits = self.dirty() && !self.pending(Instant::now());
        match self.draft.as_mut() {
            Some(draft) if keep_edits => draft.active_profile.clone_from(&config.active_profile),
            _ => self.draft = Some(config.clone()),
        }
        self.config = Some(config);
        self.channels = channels;
        self.pending_apply = false;
        self.pending_since = None;
    }

    /// Marks a `SetConfig` as sent: the UI shows it pending until the answer or 5 s.
    pub fn start_pending(&mut self) {
        self.pending_apply = true;
        self.pending_since = Some(Instant::now());
    }

    /// A sent config awaits the service answer (at most 5 s).
    pub fn pending(&self, now: Instant) -> bool {
        self.pending_apply
            && self
                .pending_since
                .is_none_or(|at| now.saturating_duration_since(at) < PENDING_TIMEOUT)
    }

    /// Instant of the last received snapshot (`None`: daemon never seen).
    pub fn last_update(&self) -> Option<Instant> {
        self.last_update
    }

    pub fn connected(&self) -> bool {
        self.last_update
            .is_some_and(|at| at.elapsed() <= STALE_AFTER)
    }

    pub fn readings(&self) -> &[Reading] {
        self.latest.as_ref().map_or(&[], |s| &s.readings)
    }

    /// The draft differs from the daemon config.
    pub fn dirty(&self) -> bool {
        self.draft.is_some() && self.draft != self.config
    }

    /// Duty applied to a channel: `None` = unknown, `Some(None)` = Auto (BIOS).
    pub fn applied_duty(&self, channel: &str) -> Option<Option<u8>> {
        self.fans
            .iter()
            .find(|f| f.channel == channel)
            .map(|f| f.duty)
    }

    /// The channel has a fan plugged in (see [`fan_connected`]).
    pub fn is_connected(&self, channel: &FanChannel) -> bool {
        let piloted = matches!(self.applied_duty(&channel.id), Some(Some(_)))
            || self
                .draft
                .as_ref()
                .is_some_and(|d| settings_for(d, &channel.id).mode != FanMode::Auto);
        fan_connected(channel_rpm(channel, self.readings()), piloted)
    }
}

/// Short mode text: "Auto", "Fixed 50 %", "Curve".
pub fn mode_label(mode: &FanMode) -> String {
    match mode {
        FanMode::Auto => "Auto".into(),
        FanMode::Fixed { duty } => format!("Fixed {duty} %"),
        FanMode::Curve { .. } => "Curve".into(),
    }
}

/// Speed read from the channel RPM sensor, if any.
pub fn channel_rpm(channel: &FanChannel, readings: &[Reading]) -> Option<f32> {
    let id = channel.rpm_sensor.as_deref()?;
    readings.iter().find(|r| r.id == id).map(|r| r.value)
}

/// Fan plugged in: controlled, or spinning. Without an RPM reading we cannot tell: plugged in.
pub fn fan_connected(rpm: Option<f32>, piloted: bool) -> bool {
    piloted || rpm.is_none_or(|rpm| rpm >= 1.0)
}

/// Pump channel: same rule as the service ("Pump Fan" on MSI, `pump` elsewhere).
pub fn is_pump(channel: &FanChannel) -> bool {
    [&channel.label, &channel.id]
        .iter()
        .any(|text| text.to_lowercase().contains("pump"))
}

/// Displayed channel name: custom name, otherwise hardware label.
pub fn display_name(config: Option<&Config>, channel: &FanChannel) -> String {
    config
        .and_then(|c| c.fan_labels.get(&channel.id))
        .cloned()
        .unwrap_or_else(|| channel.label.clone())
}

/// Channel family, to group the list (display order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FanGroup {
    Motherboard,
    Gpu,
    Watercooling,
}

impl FanGroup {
    pub const ALL: [Self; 3] = [Self::Motherboard, Self::Gpu, Self::Watercooling];

    pub fn title(self) -> &'static str {
        match self {
            Self::Motherboard => "Motherboard",
            Self::Gpu => "GPU",
            Self::Watercooling => "Watercooling",
        }
    }
}

/// Family inferred from the channel id (`gpu0/fan0`, `nct6687/pwm1`, `aio/pump`...).
/// Super I/O chips have varied names: any unknown id goes to "Motherboard".
pub fn fan_group(channel_id: &str) -> FanGroup {
    let id = channel_id.to_ascii_lowercase();
    if id.starts_with("gpu") {
        FanGroup::Gpu
    } else if ["aio", "pump", "kraken", "usb", "cooler"]
        .iter()
        .any(|k| id.contains(k))
    {
        FanGroup::Watercooling
    } else {
        FanGroup::Motherboard
    }
}

/// Channel settings in the active profile (Auto by default).
pub fn settings_for(config: &Config, channel: &str) -> FanSettings {
    config
        .active()
        .and_then(|p| p.fans.get(channel))
        .cloned()
        .unwrap_or_default()
}

/// Mutable channel settings in the active profile (created if missing).
pub fn settings_mut<'a>(config: &'a mut Config, channel: &str) -> Option<&'a mut FanSettings> {
    active_mut(config).map(|p| p.fans.entry(channel.to_owned()).or_default())
}

/// Mutable active profile.
fn active_mut(config: &mut Config) -> Option<&mut Profile> {
    let active = config.active_profile.clone();
    config.profiles.iter_mut().find(|p| p.name == active)
}

/// Puts a channel back to Auto (BIOS) in the active profile. `true` if something changed.
pub fn reset_fan(config: &mut Config, channel: &str) -> bool {
    active_mut(config).is_some_and(|p| p.fans.remove(channel).is_some())
}

/// Clears the active profile: all its channels go back to Auto (BIOS). `true` if something changed.
pub fn reset_active_profile(config: &mut Config) -> bool {
    active_mut(config).is_some_and(|p| {
        let changed = !p.fans.is_empty();
        p.fans.clear();
        changed
    })
}

/// Offered temperature sources: every temperature sensor, plus "Max CPU/GPU".
pub fn temp_sources(readings: &[Reading]) -> Vec<(String, TempSource)> {
    let temps = readings
        .iter()
        .filter(|r| r.kind == SensorKind::Temperature);
    let cpu = temps.clone().find(|r| r.device == Device::Cpu);
    let gpu = temps.clone().find(|r| r.id == GPU_TEMP);
    let mut sources = Vec::new();
    if let (Some(cpu), Some(gpu)) = (cpu, gpu) {
        sources.push((
            "Max CPU/GPU".to_owned(),
            TempSource::Max {
                ids: vec![cpu.id.clone(), gpu.id.clone()],
            },
        ));
    }
    sources.extend(temps.map(|r| (r.label.clone(), TempSource::Sensor { id: r.id.clone() })));
    sources
}

/// Valid profile name: not empty and not already taken.
fn check_new_name(config: &Config, name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a profile name.".into());
    }
    if config.profiles.iter().any(|p| p.name == name) {
        return Err(format!("Profile \"{name}\" already exists."));
    }
    Ok(name.to_owned())
}

/// Copies the active profile under a new name and activates it.
pub fn duplicate_active(config: &mut Config, name: &str) -> Result<(), String> {
    let name = check_new_name(config, name)?;
    let fans = config.active().map(|p| p.fans.clone()).unwrap_or_default();
    config.profiles.push(Profile {
        name: name.clone(),
        fans,
    });
    config.active_profile = name;
    Ok(())
}

/// Renames the active profile.
pub fn rename_active(config: &mut Config, name: &str) -> Result<(), String> {
    let name = check_new_name(config, name)?;
    let profile = active_mut(config).ok_or("No active profile.")?;
    let old = std::mem::replace(&mut profile.name, name.clone());
    config.active_profile = name;
    let rules = &mut config.automations;
    let targets = rules.process_rules.iter_mut().map(|r| &mut r.profile);
    for target in targets.chain(rules.schedules.iter_mut().map(|r| &mut r.profile)) {
        if *target == old {
            target.clone_from(&config.active_profile);
        }
    }
    Ok(())
}

/// Deletes a profile (never the last one); the active profile falls back to the first remaining.
pub fn delete_profile(config: &mut Config, name: &str) -> Result<(), String> {
    if config.profiles.len() <= 1 {
        return Err("Cannot delete the last profile.".into());
    }
    config.profiles.retain(|p| p.name != name);
    let rules = &mut config.automations;
    rules.process_rules.retain(|r| r.profile != name);
    rules.schedules.retain(|r| r.profile != name);
    if config.active_profile == name {
        config.active_profile = config.profiles[0].name.clone();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_core::config::{ProcessRule, ScheduleRule};

    fn channel(id: &str) -> FanChannel {
        FanChannel {
            id: id.into(),
            label: format!("Fan {id}"),
            rpm_sensor: None,
        }
    }

    fn temp(id: &str, device: Device) -> Reading {
        Reading {
            id: id.into(),
            label: id.into(),
            device,
            kind: SensorKind::Temperature,
            value: 40.0,
        }
    }

    #[test]
    fn channels_are_grouped_by_id() {
        assert_eq!(fan_group("gpu0/fan1"), FanGroup::Gpu);
        assert_eq!(fan_group("nct6687/pwm8"), FanGroup::Motherboard);
        assert_eq!(fan_group("it8689/fan2"), FanGroup::Motherboard);
        assert_eq!(fan_group("aio/pump"), FanGroup::Watercooling);
        assert_eq!(fan_group("usb-kraken/fan1"), FanGroup::Watercooling);
    }

    #[test]
    fn display_name_prefers_custom_label() {
        let mut config = Config::default();
        assert_eq!(display_name(Some(&config), &channel("a")), "Fan a");
        config.fan_labels.insert("a".into(), "Front".into());
        assert_eq!(display_name(Some(&config), &channel("a")), "Front");
        assert_eq!(display_name(None, &channel("a")), "Fan a");
    }

    #[test]
    fn settings_default_to_auto_and_edit_active_profile() {
        let mut config = Config::default();
        assert_eq!(settings_for(&config, "a").mode, FanMode::Auto);
        if let Some(s) = settings_mut(&mut config, "a") {
            s.mode = FanMode::Fixed { duty: 40 };
        }
        assert_eq!(settings_for(&config, "a").mode, FanMode::Fixed { duty: 40 });
        config.active_profile = "Silent".into();
        assert_eq!(settings_for(&config, "a").mode, FanMode::Auto);
    }

    #[test]
    fn draft_vs_state() {
        let mut state = HubState::default();
        assert!(!state.dirty());
        let channels = vec![channel("a")];
        state.receive(DaemonMsg::State {
            config: Config::default(),
            channels: channels.clone(),
            auto_profile: None,
        });
        assert!(!state.dirty());
        if let Some(draft) = state.draft.as_mut() {
            draft.poll_interval_ms = 1500;
        }
        assert!(state.dirty());
        // Profile switch on the daemon side: the local edit is kept.
        let switched = Config {
            active_profile: "Silent".into(),
            ..Config::default()
        };
        state.receive(DaemonMsg::State {
            config: switched.clone(),
            channels: channels.clone(),
            auto_profile: None,
        });
        let draft = state.draft.clone().unwrap_or_default();
        assert_eq!(draft.poll_interval_ms, 1500);
        assert_eq!(draft.active_profile, "Silent");
        // After "Apply", the daemon answer replaces the draft.
        state.pending_apply = true;
        state.receive(DaemonMsg::State {
            config: switched,
            channels,
            auto_profile: None,
        });
        assert!(!state.dirty());
    }

    #[test]
    fn pending_clears_on_answer_or_timeout() {
        let mut state = HubState::default();
        let now = Instant::now();
        assert!(!state.pending(now));
        state.start_pending();
        assert!(state.pending(now));
        assert!(!state.pending(now + Duration::from_secs(6)));
        state.receive(DaemonMsg::State {
            config: Config::default(),
            channels: Vec::new(),
            auto_profile: None,
        });
        assert!(!state.pending(now));
        state.start_pending();
        state.receive(DaemonMsg::Error {
            message: "x".into(),
        });
        assert!(!state.pending(now));
    }

    #[test]
    fn timed_out_apply_keeps_local_edits() {
        let mut state = HubState::default();
        state.receive(DaemonMsg::State {
            config: Config::default(),
            channels: Vec::new(),
            auto_profile: None,
        });
        if let Some(draft) = state.draft.as_mut() {
            draft.poll_interval_ms = 1500;
        }
        state.start_pending();
        state.pending_since = Instant::now().checked_sub(Duration::from_secs(6));
        state.receive(DaemonMsg::State {
            config: Config::default(),
            channels: Vec::new(),
            auto_profile: None,
        });
        assert_eq!(state.draft.map(|d| d.poll_interval_ms), Some(1500));
    }

    #[test]
    fn max_source_needs_cpu_and_gpu() {
        let cpu = temp("cpu/package_temp", Device::Cpu);
        let gpu = temp(GPU_TEMP, Device::Gpu);
        let sources = temp_sources(&[cpu.clone(), gpu]);
        assert_eq!(sources.len(), 3);
        assert_eq!(
            sources[0].1,
            TempSource::Max {
                ids: vec!["cpu/package_temp".into(), GPU_TEMP.into()]
            }
        );
        assert_eq!(temp_sources(&[cpu]).len(), 1);
    }

    #[test]
    fn profile_operations() {
        let mut config = Config::default();
        assert!(duplicate_active(&mut config, "  ").is_err());
        assert!(duplicate_active(&mut config, "Silent").is_err());
        assert!(duplicate_active(&mut config, "Gaming").is_ok());
        assert_eq!(config.active_profile, "Gaming");
        assert!(rename_active(&mut config, "Games").is_ok());
        assert_eq!(config.active_profile, "Games");
        assert!(config.profiles.iter().any(|p| p.name == "Games"));
        assert!(delete_profile(&mut config, "Games").is_ok());
        assert_eq!(config.active_profile, config.profiles[0].name);
        let mut single = Config {
            profiles: vec![Profile {
                name: "Only".into(),
                fans: Default::default(),
            }],
            ..Config::default()
        };
        assert!(delete_profile(&mut single, "Only").is_err());
    }

    #[test]
    fn profile_changes_follow_automations() {
        let mut config = Config::default();
        config.automations.process_rules.push(ProcessRule {
            exe: "game.exe".into(),
            profile: "Balanced".into(),
        });
        config.automations.schedules.push(ScheduleRule {
            start: 0,
            end: 60,
            profile: "Balanced".into(),
        });
        config.automations.schedules.push(ScheduleRule {
            start: 60,
            end: 120,
            profile: "Silent".into(),
        });
        assert!(rename_active(&mut config, "Main").is_ok());
        assert_eq!(config.automations.process_rules[0].profile, "Main");
        assert_eq!(config.automations.schedules[0].profile, "Main");
        assert_eq!(config.automations.schedules[1].profile, "Silent");
        assert!(delete_profile(&mut config, "Main").is_ok());
        assert!(config.automations.process_rules.is_empty());
        assert_eq!(config.automations.schedules.len(), 1);
    }

    #[test]
    fn reset_fan_and_profile_back_to_auto() {
        let mut config = Config::default();
        if let Some(s) = settings_mut(&mut config, "a") {
            s.mode = FanMode::Fixed { duty: 40 };
        }
        settings_mut(&mut config, "b");
        assert!(reset_fan(&mut config, "a"));
        assert_eq!(settings_for(&config, "a"), FanSettings::default());
        assert!(!reset_fan(&mut config, "a"));
        assert!(reset_active_profile(&mut config));
        assert!(config.active().is_some_and(|p| p.fans.is_empty()));
        assert!(!reset_active_profile(&mut config));
        // Other profiles are left untouched.
        config.active_profile = "Silent".into();
        settings_mut(&mut config, "a");
        config.active_profile = "Performance".into();
        settings_mut(&mut config, "a");
        assert!(reset_active_profile(&mut config));
        config.active_profile = "Silent".into();
        assert!(config.active().is_some_and(|p| p.fans.contains_key("a")));
    }

    #[test]
    fn connected_fans_and_pumps() {
        assert!(fan_connected(Some(800.0), false));
        assert!(!fan_connected(Some(0.0), false));
        assert!(fan_connected(Some(0.0), true));
        assert!(fan_connected(None, false));
        let pump = FanChannel {
            id: "nct6687/pwm2".into(),
            label: "Pump Fan".into(),
            rpm_sensor: Some("rpm2".into()),
        };
        assert!(is_pump(&pump));
        assert!(!is_pump(&channel("nct6687/pwm3")));
        let rpm = Reading {
            id: "rpm2".into(),
            label: "Pump".into(),
            device: Device::Motherboard,
            kind: SensorKind::Fan,
            value: 2400.0,
        };
        assert_eq!(channel_rpm(&pump, &[rpm]), Some(2400.0));
        assert_eq!(channel_rpm(&pump, &[]), None);
    }
}
