//! Configuration persistée par le daemon : profils, réglages par ventilo, failsafe.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Point de courbe : à `temp` °C, le ventilo tourne à `duty` %.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub temp: f32,
    pub duty: u8,
}

/// Température qui pilote un ventilo (ids de `Reading`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TempSource {
    Sensor { id: String },
    Max { ids: Vec<String> },
    Average { ids: Vec<String> },
}

/// Mode de pilotage d'un ventilo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum FanMode {
    /// Rendu au BIOS / firmware : le daemon n'écrit rien.
    Auto,
    Fixed {
        duty: u8,
    },
    Curve {
        source: TempSource,
        points: Vec<CurvePoint>,
    },
}

/// Réglages d'un canal de ventilo dans un profil.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FanSettings {
    pub mode: FanMode,
    /// Plancher en % hors mode Auto (pompe AIO, ventirad CPU).
    pub min_duty: u8,
    /// Sous cette température, arrêt complet (0 %) au lieu du plancher.
    pub zero_rpm_below: Option<f32>,
    /// Écart de température minimal (°C) avant de recalculer la consigne.
    pub hysteresis: f32,
    /// Variation maximale de consigne par tick, à la hausse et à la baisse (%).
    pub step_up: u8,
    pub step_down: u8,
    /// Délai minimal entre deux changements de consigne.
    pub response_ms: u64,
}

impl Default for FanSettings {
    fn default() -> Self {
        Self {
            mode: FanMode::Auto,
            min_duty: 20,
            zero_rpm_below: None,
            hysteresis: 2.0,
            step_up: 10,
            step_down: 5,
            response_ms: 1000,
        }
    }
}

/// Profil nommé. Un canal absent de `fans` reste en Auto.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub fans: BTreeMap<String, FanSettings>,
}

/// Seuils critiques : au-delà, tous les ventilos pilotés passent à 100 %.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FailsafeConfig {
    pub cpu_critical: f32,
    pub gpu_critical: f32,
}

impl Default for FailsafeConfig {
    fn default() -> Self {
        Self {
            cpu_critical: 90.0,
            gpu_critical: 85.0,
        }
    }
}

/// Configuration complète, fichier `config.toml` du daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub poll_interval_ms: u64,
    pub active_profile: String,
    pub profiles: Vec<Profile>,
    /// Noms personnalisés des canaux (« Avant 1 », « CPU Cooler »), communs à tous les profils.
    pub fan_labels: BTreeMap<String, String>,
    pub failsafe: FailsafeConfig,
    /// Éclairage, commun à tous les profils.
    pub rgb: crate::rgb::RgbConfig,
}

impl Default for Config {
    fn default() -> Self {
        let profile = |name: &str| Profile {
            name: name.into(),
            fans: BTreeMap::new(),
        };
        Self {
            poll_interval_ms: 1000,
            active_profile: "Équilibré".into(),
            profiles: ["Silencieux", "Équilibré", "Performance", "Custom"]
                .map(profile)
                .into(),
            fan_labels: BTreeMap::new(),
            failsafe: FailsafeConfig::default(),
            rgb: crate::rgb::RgbConfig::default(),
        }
    }
}

/// Erreur de configuration invalide (renvoyée à l'UI).
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("configuration invalide : {0}")]
    Invalid(String),
    #[error("TOML illisible : {0}")]
    Parse(#[from] toml::de::Error),
    #[error("TOML non sérialisable : {0}")]
    Serialize(#[from] toml::ser::Error),
}

impl Config {
    /// Profil actif, s'il existe.
    pub fn active(&self) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == self.active_profile)
    }

    /// Normalise et vérifie la config reçue (UI ou fichier) avant de l'appliquer :
    /// bornes 0..=100, points triés par température, profil actif existant, intervalle 500..=2000 ms…
    pub fn validate(&mut self) -> Result<(), ConfigError> {
        self.poll_interval_ms = self.poll_interval_ms.clamp(500, 2000);
        if self.profiles.is_empty() {
            return Err(ConfigError::Invalid("aucun profil défini".into()));
        }
        for (i, profile) in self.profiles.iter().enumerate() {
            if profile.name.trim().is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "le profil n°{} n'a pas de nom",
                    i + 1
                )));
            }
            if self.profiles[..i].iter().any(|p| p.name == profile.name) {
                return Err(ConfigError::Invalid(format!(
                    "nom de profil en double : « {} »",
                    profile.name
                )));
            }
        }
        if self.active().is_none() {
            return Err(ConfigError::Invalid(format!(
                "profil actif inconnu : « {} »",
                self.active_profile
            )));
        }
        for profile in &mut self.profiles {
            for (channel, fan) in &mut profile.fans {
                fan.validate().map_err(|e| {
                    ConfigError::Invalid(format!(
                        "profil « {} », canal « {channel} » : {e}",
                        profile.name
                    ))
                })?;
            }
        }
        let fs = &mut self.failsafe;
        fs.cpu_critical = clamp_or(
            fs.cpu_critical,
            50.0,
            110.0,
            FailsafeConfig::default().cpu_critical,
        );
        fs.gpu_critical = clamp_or(
            fs.gpu_critical,
            50.0,
            110.0,
            FailsafeConfig::default().gpu_critical,
        );
        self.rgb.validate()?;
        Ok(())
    }

    /// Lit puis valide une config TOML.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let mut config: Self = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    pub fn to_toml(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }
}

impl FanSettings {
    /// Normalise un réglage de ventilo ; renvoie le motif du refus en français.
    fn validate(&mut self) -> Result<(), String> {
        self.min_duty = self.min_duty.min(100);
        self.hysteresis = clamp_or(self.hysteresis, 0.0, 10.0, 2.0);
        self.zero_rpm_below = self.zero_rpm_below.filter(|t| t.is_finite());
        match &mut self.mode {
            FanMode::Auto => {}
            FanMode::Fixed { duty } => *duty = (*duty).min(100),
            FanMode::Curve { source, points } => {
                let ids = match source {
                    TempSource::Sensor { id } => std::slice::from_ref(&*id),
                    TempSource::Max { ids } | TempSource::Average { ids } => ids.as_slice(),
                };
                if ids.is_empty() || ids.iter().any(|id| id.trim().is_empty()) {
                    return Err("source de température sans capteur".into());
                }
                if points.is_empty() {
                    return Err("la courbe doit contenir au moins un point".into());
                }
                if points.iter().any(|p| !p.temp.is_finite()) {
                    return Err("température de point de courbe invalide".into());
                }
                // Tri stable : à température égale, le dernier point saisi l'emporte.
                points.sort_by(|a, b| a.temp.total_cmp(&b.temp));
                let mut unique: Vec<CurvePoint> = Vec::with_capacity(points.len());
                for p in points.drain(..) {
                    let duty = p.duty.min(100);
                    match unique.last_mut() {
                        Some(last) if last.temp == p.temp => last.duty = duty,
                        _ => unique.push(CurvePoint { temp: p.temp, duty }),
                    }
                }
                *points = unique;
            }
        }
        Ok(())
    }
}

/// Borne `value` ; NaN → `fallback`.
fn clamp_or(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if value.is_nan() {
        fallback
    } else {
        value.clamp(min, max)
    }
}

/// Courbes prédéfinies proposées dans l'éditeur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Preset {
    Silent,
    Balanced,
    Performance,
}

impl Preset {
    /// Mêmes courbes que les profils livrés (`configs/controlanything-profils.toml`).
    pub fn points(self) -> Vec<CurvePoint> {
        let pts: &[(f32, u8)] = match self {
            Self::Silent => &[(45.0, 25), (65.0, 32), (78.0, 48), (88.0, 70), (95.0, 100)],
            Self::Balanced => &[(40.0, 30), (60.0, 40), (75.0, 60), (85.0, 85), (92.0, 100)],
            Self::Performance => &[(40.0, 40), (60.0, 55), (75.0, 80), (85.0, 100)],
        };
        pts.iter()
            .map(|&(temp, duty)| CurvePoint { temp, duty })
            .collect()
    }

    /// Applique la recette complète (courbe + lissage), en gardant la source de température.
    pub fn apply(self, settings: &mut FanSettings) {
        let (min_duty, hysteresis, step_up, step_down, response_ms) = match self {
            Self::Silent => (25, 6.0, 3, 2, 3000),
            Self::Balanced => (30, 4.0, 5, 3, 2000),
            Self::Performance => (40, 3.0, 10, 4, 1000),
        };
        if let FanMode::Curve { points, .. } = &mut settings.mode {
            *points = self.points();
        }
        settings.min_duty = min_duty;
        settings.hysteresis = hysteresis;
        settings.step_up = step_up;
        settings.step_down = step_down;
        settings.response_ms = response_ms;
    }

    /// Vrai si `settings` correspond exactement à ce préréglage.
    pub fn matches(self, settings: &FanSettings) -> bool {
        let mut expected = settings.clone();
        self.apply(&mut expected);
        expected == *settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve_fan(points: Vec<CurvePoint>) -> FanSettings {
        FanSettings {
            mode: FanMode::Curve {
                source: TempSource::Sensor {
                    id: "cpu/package_temp".into(),
                },
                points,
            },
            ..FanSettings::default()
        }
    }

    fn with_fan(fan: FanSettings) -> Config {
        let mut config = Config::default();
        config.profiles[1].fans.insert("nct6687/pwm1".into(), fan);
        config
    }

    fn pt(temp: f32, duty: u8) -> CurvePoint {
        CurvePoint { temp, duty }
    }

    fn fan_of(config: &Config) -> &FanSettings {
        &config.profiles[1].fans["nct6687/pwm1"]
    }

    #[test]
    fn validate_sorts_dedupes_and_clamps() {
        let mut fan = curve_fan(vec![
            pt(70.0, 150),
            pt(30.0, 20),
            pt(50.0, 40),
            pt(30.0, 25),
        ]);
        fan.min_duty = 200;
        fan.hysteresis = 50.0;
        let mut config = with_fan(fan);
        config.poll_interval_ms = 10;
        config.failsafe = FailsafeConfig {
            cpu_critical: 200.0,
            gpu_critical: 10.0,
        };
        config.validate().expect("valide");
        let fan = fan_of(&config);
        let FanMode::Curve { points, .. } = &fan.mode else {
            panic!("mode courbe attendu")
        };
        assert_eq!(points, &vec![pt(30.0, 25), pt(50.0, 40), pt(70.0, 100)]);
        assert_eq!(fan.min_duty, 100);
        assert_eq!(fan.hysteresis, 10.0);
        assert_eq!(config.poll_interval_ms, 500);
        assert_eq!(
            config.failsafe,
            FailsafeConfig {
                cpu_critical: 110.0,
                gpu_critical: 50.0
            }
        );

        let fan = FanSettings {
            mode: FanMode::Fixed { duty: 101 },
            hysteresis: f32::NAN,
            ..FanSettings::default()
        };
        let mut config = with_fan(fan);
        config.poll_interval_ms = 9000;
        config.validate().expect("valide");
        assert_eq!(fan_of(&config).mode, FanMode::Fixed { duty: 100 });
        assert_eq!(fan_of(&config).hysteresis, 2.0);
        assert_eq!(config.poll_interval_ms, 2000);
    }

    fn assert_invalid(mut config: Config, needle: &str) {
        match config.validate() {
            Err(ConfigError::Invalid(msg)) => assert!(msg.contains(needle), "{msg}"),
            other => panic!("attendu Invalid({needle}), obtenu {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_bad_configs() {
        assert_invalid(
            Config {
                profiles: vec![],
                ..Config::default()
            },
            "aucun profil",
        );
        let mut config = Config::default();
        config.profiles[0].name = " ".into();
        assert_invalid(config, "n°1");
        let mut config = Config::default();
        config.profiles[2].name = "Silencieux".into();
        assert_invalid(config, "Silencieux");
        assert_invalid(
            Config {
                active_profile: "Nope".into(),
                ..Config::default()
            },
            "Nope",
        );
        assert_invalid(with_fan(curve_fan(vec![])), "nct6687/pwm1");
        assert_invalid(with_fan(curve_fan(vec![pt(f32::NAN, 50)])), "Équilibré");
        let empty_ids = FanSettings {
            mode: FanMode::Curve {
                source: TempSource::Max { ids: vec![] },
                points: vec![pt(40.0, 50)],
            },
            ..FanSettings::default()
        };
        assert_invalid(with_fan(empty_ids), "source");
        let empty_id = FanSettings {
            mode: FanMode::Curve {
                source: TempSource::Sensor { id: String::new() },
                points: vec![pt(40.0, 50)],
            },
            ..FanSettings::default()
        };
        assert_invalid(with_fan(empty_id), "source");
    }

    #[test]
    fn default_config_roundtrips_through_toml() {
        let config = Config::default();
        let text = config.to_toml().expect("sérialisation");
        assert_eq!(Config::from_toml(&text).expect("lecture"), config);
    }

    #[test]
    fn full_config_roundtrips_through_toml() {
        let mut config = with_fan(FanSettings {
            mode: FanMode::Curve {
                source: TempSource::Max {
                    ids: vec!["cpu/package_temp".into(), "gpu0/core_temp".into()],
                },
                points: Preset::Balanced.points(),
            },
            zero_rpm_below: Some(35.5),
            ..FanSettings::default()
        });
        let fans = &mut config.profiles[1].fans;
        fans.insert(
            "gpu0/fan0".into(),
            FanSettings {
                mode: FanMode::Fixed { duty: 60 },
                ..FanSettings::default()
            },
        );
        fans.insert("nct6687/pwm2".into(), FanSettings::default());
        fans.insert(
            "nct6687/pwm3".into(),
            FanSettings {
                mode: FanMode::Curve {
                    source: TempSource::Average {
                        ids: vec!["a".into(), "b".into()],
                    },
                    points: Preset::Silent.points(),
                },
                ..FanSettings::default()
            },
        );
        config
            .fan_labels
            .insert("nct6687/pwm1".into(), "CPU Cooler".into());
        config.validate().expect("valide");
        let text = config.to_toml().expect("sérialisation");
        assert_eq!(Config::from_toml(&text).expect("lecture"), config, "{text}");
    }

    #[test]
    fn partial_toml_uses_defaults() {
        let text = r#"
active_profile = "Perso"

[[profiles]]
name = "Perso"

[profiles.fans."nct6687/pwm1"]
min_duty = 30

[profiles.fans."nct6687/pwm1".mode]
mode = "curve"
source = { type = "sensor", id = "cpu/package_temp" }
points = [{ temp = 60.0, duty = 80 }, { temp = 30.0, duty = 20 }]
"#;
        let config = Config::from_toml(text).expect("lecture");
        let fan = &config.profiles[0].fans["nct6687/pwm1"];
        assert_eq!(fan.min_duty, 30);
        assert_eq!(fan.step_up, FanSettings::default().step_up);
        assert_eq!(fan.zero_rpm_below, None);
        let FanMode::Curve { points, .. } = &fan.mode else {
            panic!("mode courbe attendu")
        };
        assert_eq!(points[0], pt(30.0, 20));
        assert!(matches!(
            Config::from_toml("poll_interval_ms = \"x\""),
            Err(ConfigError::Parse(_))
        ));
    }
}
