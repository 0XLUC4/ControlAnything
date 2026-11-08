//! État partagé de l'UI : dernier message du daemon, brouillon de config et règles d'édition pures.

use std::time::{Duration, Instant};

use hub_core::config::{Config, FanMode, FanSettings, Profile, TempSource};
use hub_core::fan::{FanChannel, FanStatus};
use hub_core::ipc::{DaemonMsg, Snapshot};
use hub_core::{Device, Reading, SensorKind};

/// Au-delà, le daemon est considéré comme déconnecté.
const STALE_AFTER: Duration = Duration::from_secs(3);
/// Id de la température GPU (backend NVIDIA).
pub const GPU_TEMP: &str = "gpu0/temp";

/// Message affiché en haut de page.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    Info(String),
    Error(String),
}

#[derive(Default)]
pub struct HubState {
    pub latest: Option<Snapshot>,
    last_update: Option<Instant>,
    /// Config confirmée par le dernier `State` du daemon.
    pub config: Option<Config>,
    /// Copie locale éditée par l'UI, envoyée par « Appliquer ».
    pub draft: Option<Config>,
    pub channels: Vec<FanChannel>,
    pub fans: Vec<FanStatus>,
    pub failsafe: bool,
    pub notice: Option<Notice>,
    /// Un `SetConfig` attend sa réponse : le prochain `State` remplace le brouillon.
    pub pending_apply: bool,
}

impl HubState {
    /// Intègre un message du daemon.
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
            DaemonMsg::State { config, channels } => self.on_state(config, channels),
            // Page Éclairage : vague suivante.
            DaemonMsg::Rgb { .. } => {}
            DaemonMsg::Error { message } => {
                self.pending_apply = false;
                self.notice = Some(Notice::Error(message));
            }
        }
    }

    fn on_state(&mut self, config: Config, channels: Vec<FanChannel>) {
        // Des modifications locales non envoyées survivent à un changement de profil.
        let keep_edits = self.dirty() && !self.pending_apply;
        match self.draft.as_mut() {
            Some(draft) if keep_edits => draft.active_profile.clone_from(&config.active_profile),
            _ => self.draft = Some(config.clone()),
        }
        self.config = Some(config);
        self.channels = channels;
        self.pending_apply = false;
    }

    /// Instant du dernier relevé reçu (`None` : daemon jamais vu).
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

    /// Le brouillon diffère de la config du daemon.
    pub fn dirty(&self) -> bool {
        self.draft.is_some() && self.draft != self.config
    }

    /// Consigne appliquée à un canal : `None` = inconnu, `Some(None)` = Auto (BIOS).
    pub fn applied_duty(&self, channel: &str) -> Option<Option<u8>> {
        self.fans
            .iter()
            .find(|f| f.channel == channel)
            .map(|f| f.duty)
    }

    /// Le canal a un ventilateur branché (voir [`fan_connected`]).
    pub fn is_connected(&self, channel: &FanChannel) -> bool {
        let piloted = matches!(self.applied_duty(&channel.id), Some(Some(_)))
            || self
                .draft
                .as_ref()
                .is_some_and(|d| settings_for(d, &channel.id).mode != FanMode::Auto);
        fan_connected(channel_rpm(channel, self.readings()), piloted)
    }
}

/// Vitesse lue sur le capteur RPM du canal, s'il existe.
pub fn channel_rpm(channel: &FanChannel, readings: &[Reading]) -> Option<f32> {
    let id = channel.rpm_sensor.as_deref()?;
    readings.iter().find(|r| r.id == id).map(|r| r.value)
}

/// Ventilateur branché : piloté, ou qui tourne. Sans mesure RPM on ne peut pas trancher : branché.
pub fn fan_connected(rpm: Option<f32>, piloted: bool) -> bool {
    piloted || rpm.is_none_or(|rpm| rpm >= 1.0)
}

/// Canal de pompe : même règle que le service (« Pump Fan » sur MSI, `pump` ailleurs).
pub fn is_pump(channel: &FanChannel) -> bool {
    [&channel.label, &channel.id]
        .iter()
        .any(|text| text.to_lowercase().contains("pump"))
}

/// Nom affiché d'un canal : nom personnalisé, sinon libellé matériel.
pub fn display_name(config: Option<&Config>, channel: &FanChannel) -> String {
    config
        .and_then(|c| c.fan_labels.get(&channel.id))
        .cloned()
        .unwrap_or_else(|| channel.label.clone())
}

/// Famille d'un canal, pour grouper la liste (ordre d'affichage).
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
            Self::Motherboard => "Carte mère",
            Self::Gpu => "GPU",
            Self::Watercooling => "Watercooling",
        }
    }
}

/// Famille déduite de l'id du canal (`gpu0/fan0`, `nct6687/pwm1`, `aio/pump`…).
/// Les puces Super I/O ont des noms variés : tout id inconnu est rangé « Carte mère ».
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

/// Réglages du canal dans le profil actif (Auto par défaut).
pub fn settings_for(config: &Config, channel: &str) -> FanSettings {
    config
        .active()
        .and_then(|p| p.fans.get(channel))
        .cloned()
        .unwrap_or_default()
}

/// Réglages modifiables du canal dans le profil actif (créés si absents).
pub fn settings_mut<'a>(config: &'a mut Config, channel: &str) -> Option<&'a mut FanSettings> {
    active_mut(config).map(|p| p.fans.entry(channel.to_owned()).or_default())
}

/// Profil actif modifiable.
fn active_mut(config: &mut Config) -> Option<&mut Profile> {
    let active = config.active_profile.clone();
    config.profiles.iter_mut().find(|p| p.name == active)
}

/// Remet un canal en Auto (BIOS) dans le profil actif. `true` si quelque chose a changé.
pub fn reset_fan(config: &mut Config, channel: &str) -> bool {
    active_mut(config).is_some_and(|p| p.fans.remove(channel).is_some())
}

/// Vide le profil actif : tous ses canaux repassent en Auto (BIOS). `true` si quelque chose a changé.
pub fn reset_active_profile(config: &mut Config) -> bool {
    active_mut(config).is_some_and(|p| {
        let changed = !p.fans.is_empty();
        p.fans.clear();
        changed
    })
}

/// Sources de température proposées : chaque capteur de température, plus « Max CPU/GPU ».
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

/// Nom de profil valide : non vide et pas déjà pris.
fn check_new_name(config: &Config, name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Saisissez un nom de profil.".into());
    }
    if config.profiles.iter().any(|p| p.name == name) {
        return Err(format!("Le profil « {name} » existe déjà."));
    }
    Ok(name.to_owned())
}

/// Copie le profil actif sous un nouveau nom et l'active.
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

/// Renomme le profil actif.
pub fn rename_active(config: &mut Config, name: &str) -> Result<(), String> {
    let name = check_new_name(config, name)?;
    let profile = active_mut(config).ok_or("Aucun profil actif.")?;
    profile.name.clone_from(&name);
    config.active_profile = name;
    Ok(())
}

/// Supprime un profil (jamais le dernier) ; le profil actif retombe sur le premier restant.
pub fn delete_profile(config: &mut Config, name: &str) -> Result<(), String> {
    if config.profiles.len() <= 1 {
        return Err("Impossible de supprimer le dernier profil.".into());
    }
    config.profiles.retain(|p| p.name != name);
    if config.active_profile == name {
        config.active_profile = config.profiles[0].name.clone();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        config.fan_labels.insert("a".into(), "Avant".into());
        assert_eq!(display_name(Some(&config), &channel("a")), "Avant");
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
        config.active_profile = "Silencieux".into();
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
        });
        assert!(!state.dirty());
        if let Some(draft) = state.draft.as_mut() {
            draft.poll_interval_ms = 1500;
        }
        assert!(state.dirty());
        // Changement de profil côté daemon : l'édition locale est conservée.
        let switched = Config { active_profile: "Silencieux".into(), ..Config::default() };
        state.receive(DaemonMsg::State {
            config: switched.clone(),
            channels: channels.clone(),
        });
        let draft = state.draft.clone().unwrap_or_default();
        assert_eq!(draft.poll_interval_ms, 1500);
        assert_eq!(draft.active_profile, "Silencieux");
        // Après « Appliquer », la réponse du daemon remplace le brouillon.
        state.pending_apply = true;
        state.receive(DaemonMsg::State {
            config: switched,
            channels,
        });
        assert!(!state.dirty());
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
        assert!(duplicate_active(&mut config, "Silencieux").is_err());
        assert!(duplicate_active(&mut config, "Jeu").is_ok());
        assert_eq!(config.active_profile, "Jeu");
        assert!(rename_active(&mut config, "Jeux").is_ok());
        assert_eq!(config.active_profile, "Jeux");
        assert!(config.profiles.iter().any(|p| p.name == "Jeux"));
        assert!(delete_profile(&mut config, "Jeux").is_ok());
        assert_eq!(config.active_profile, config.profiles[0].name);
        let mut single = Config {
            profiles: vec![Profile {
                name: "Seul".into(),
                fans: Default::default(),
            }],
            ..Config::default()
        };
        assert!(delete_profile(&mut single, "Seul").is_err());
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
        // Les autres profils ne sont pas touchés.
        config.active_profile = "Silencieux".into();
        settings_mut(&mut config, "a");
        config.active_profile = "Performance".into();
        settings_mut(&mut config, "a");
        assert!(reset_active_profile(&mut config));
        config.active_profile = "Silencieux".into();
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
