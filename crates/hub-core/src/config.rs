//! Configuration persisted by the daemon: profiles, per-fan settings, failsafe.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Curve point: at `temp` °C, the fan runs at `duty` %.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub temp: f32,
    pub duty: u8,
}

/// Temperature driving a fan (`Reading` ids).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TempSource {
    Sensor { id: String },
    Max { ids: Vec<String> },
    Average { ids: Vec<String> },
}

/// Fan control mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum FanMode {
    /// Handed back to the BIOS / firmware: the daemon writes nothing.
    Auto,
    Fixed {
        duty: u8,
    },
    Curve {
        source: TempSource,
        points: Vec<CurvePoint>,
    },
}

/// Settings of one fan channel in a profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FanSettings {
    pub mode: FanMode,
    /// Floor in % outside Auto mode (AIO pump, CPU cooler).
    pub min_duty: u8,
    /// Below this temperature, full stop (0 %) instead of the floor.
    pub zero_rpm_below: Option<f32>,
    /// Minimum temperature change (°C) before recomputing the target.
    pub hysteresis: f32,
    /// Maximum target change per tick, upward and downward (%).
    pub step_up: u8,
    pub step_down: u8,
    /// Minimum delay between two target changes.
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

/// Named profile. A channel missing from `fans` stays in Auto.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub fans: BTreeMap<String, FanSettings>,
}

/// Critical thresholds: above them, every controlled fan goes to 100 %.
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

/// Full configuration, the daemon's `config.toml` file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub poll_interval_ms: u64,
    pub active_profile: String,
    pub profiles: Vec<Profile>,
    /// Custom channel names ("Front 1", "CPU Cooler"), shared by all profiles.
    pub fan_labels: BTreeMap<String, String>,
    pub failsafe: FailsafeConfig,
    /// Lighting, shared by all profiles.
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
            active_profile: "Balanced".into(),
            profiles: ["Silent", "Balanced", "Performance", "Custom"]
                .map(profile)
                .into(),
            fan_labels: BTreeMap::new(),
            failsafe: FailsafeConfig::default(),
            rgb: crate::rgb::RgbConfig::default(),
        }
    }
}

/// Invalid configuration error (sent back to the UI).
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid configuration: {0}")]
    Invalid(String),
    #[error("unreadable TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("TOML serialization failed: {0}")]
    Serialize(#[from] toml::ser::Error),
}

impl Config {
    /// Active profile, if it exists.
    pub fn active(&self) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == self.active_profile)
    }

    /// Normalizes and checks a received config (UI or file) before applying it:
    /// 0..=100 bounds, points sorted by temperature, existing active profile, 500..=2000 ms interval, etc.
    pub fn validate(&mut self) -> Result<(), ConfigError> {
        self.poll_interval_ms = self.poll_interval_ms.clamp(500, 2000);
        if self.profiles.is_empty() {
            return Err(ConfigError::Invalid("no profile defined".into()));
        }
        for (i, profile) in self.profiles.iter().enumerate() {
            if profile.name.trim().is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "profile #{} has no name",
                    i + 1
                )));
            }
            if self.profiles[..i].iter().any(|p| p.name == profile.name) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate profile name: \"{}\"",
                    profile.name
                )));
            }
        }
        if self.active().is_none() {
            return Err(ConfigError::Invalid(format!(
                "unknown active profile: \"{}\"",
                self.active_profile
            )));
        }
        for profile in &mut self.profiles {
            for (channel, fan) in &mut profile.fans {
                fan.validate().map_err(|e| {
                    ConfigError::Invalid(format!(
                        "profile \"{}\", channel \"{channel}\": {e}",
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

    /// Parses then validates a TOML config.
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
    /// Normalizes a fan setting; returns the rejection reason.
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
                    return Err("temperature source has no sensor".into());
                }
                if points.is_empty() {
                    return Err("the curve must contain at least one point".into());
                }
                if points.iter().any(|p| !p.temp.is_finite()) {
                    return Err("invalid curve point temperature".into());
                }
                // Stable sort: at equal temperature, the last entered point wins.
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

/// Clamps `value`; NaN gives `fallback`.
fn clamp_or(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if value.is_nan() {
        fallback
    } else {
        value.clamp(min, max)
    }
}

/// Predefined curves offered in the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Preset {
    Silent,
    Balanced,
    Performance,
}

impl Preset {
    /// Same curves as the shipped profiles (`configs/controlanything-profiles.toml`).
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

    /// Applies the full recipe (curve + smoothing), keeping the temperature source.
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

    /// True if `settings` exactly matches this preset.
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
        config.validate().expect("valid");
        let fan = fan_of(&config);
        let FanMode::Curve { points, .. } = &fan.mode else {
            panic!("curve mode expected")
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
        config.validate().expect("valid");
        assert_eq!(fan_of(&config).mode, FanMode::Fixed { duty: 100 });
        assert_eq!(fan_of(&config).hysteresis, 2.0);
        assert_eq!(config.poll_interval_ms, 2000);
    }

    fn assert_invalid(mut config: Config, needle: &str) {
        match config.validate() {
            Err(ConfigError::Invalid(msg)) => assert!(msg.contains(needle), "{msg}"),
            other => panic!("expected Invalid({needle}), got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_bad_configs() {
        assert_invalid(
            Config {
                profiles: vec![],
                ..Config::default()
            },
            "no profile",
        );
        let mut config = Config::default();
        config.profiles[0].name = " ".into();
        assert_invalid(config, "#1");
        let mut config = Config::default();
        config.profiles[2].name = "Silent".into();
        assert_invalid(config, "Silent");
        assert_invalid(
            Config {
                active_profile: "Nope".into(),
                ..Config::default()
            },
            "Nope",
        );
        assert_invalid(with_fan(curve_fan(vec![])), "nct6687/pwm1");
        assert_invalid(with_fan(curve_fan(vec![pt(f32::NAN, 50)])), "Balanced");
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
        let text = config.to_toml().expect("serialization");
        assert_eq!(Config::from_toml(&text).expect("parse"), config);
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
        config.validate().expect("valid");
        let text = config.to_toml().expect("serialization");
        assert_eq!(Config::from_toml(&text).expect("parse"), config, "{text}");
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
        let config = Config::from_toml(text).expect("parse");
        let fan = &config.profiles[0].fans["nct6687/pwm1"];
        assert_eq!(fan.min_duty, 30);
        assert_eq!(fan.step_up, FanSettings::default().step_up);
        assert_eq!(fan.zero_rpm_below, None);
        let FanMode::Curve { points, .. } = &fan.mode else {
            panic!("curve mode expected")
        };
        assert_eq!(points[0], pt(30.0, 20));
        assert!(matches!(
            Config::from_toml("poll_interval_ms = \"x\""),
            Err(ConfigError::Parse(_))
        ));
    }
}
