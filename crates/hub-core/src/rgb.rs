//! RGB lighting contract: devices, zones, effects.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::HwError;
use crate::config::TempSource;

/// 24-bit color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Self = Self { r: 0, g: 0, b: 0 };
    pub const WHITE: Self = Self { r: 255, g: 255, b: 255 };

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// Device zone (JRGB header, JARGB header, logo, etc.). `leds` = 1 if not addressable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RgbZone {
    /// Stable id, unique within the device, e.g. `jargb1`.
    pub id: String,
    pub label: String,
    pub leds: usize,
}

/// Detected RGB device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RgbDeviceInfo {
    /// Stable id, e.g. `mystic-light/1462:7e06`.
    pub id: String,
    pub label: String,
    pub zones: Vec<RgbZone>,
}

/// RGB device driver.
///
/// Implementations write nothing on open, save the original state before
/// the first write and call `restore` in their `Drop`.
pub trait RgbDevice: Send {
    fn info(&self) -> &RgbDeviceInfo;
    /// Applies a full frame: `frame[zone][led]`, in the order of `info().zones`.
    /// Called up to ~30 times per second: the implementation skips identical frames.
    fn set_frame(&mut self, frame: &[Vec<Rgb>]) -> Result<(), HwError>;
    /// Hands the lighting back to the firmware (original effect); must never panic.
    fn restore(&mut self);
}

/// Effect applied to a zone (or to everything, in sync mode).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum Effect {
    /// Handed back to the firmware: the daemon writes nothing.
    Firmware,
    Off,
    Static { color: Rgb },
    Breathing { color: Rgb, period_ms: u32 },
    Rainbow { period_ms: u32 },
    /// Gradient scrolling along the LEDs.
    Wave { colors: Vec<Rgb>, period_ms: u32 },
    /// Color interpolated between `cold` (<= `min_temp`) and `hot` (>= `max_temp`).
    Temperature { source: TempSource, cold: Rgb, hot: Rgb, min_temp: f32, max_temp: f32 },
    /// Color driven by a load reading (%), e.g. `cpu/load`.
    Load { sensor: String, low: Rgb, high: Rgb },
}

/// RGB settings, shared by all profiles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RgbConfig {
    /// Global brightness 0..=100 %.
    pub brightness: u8,
    /// `Some` = same effect everywhere (global sync).
    pub sync: Option<Effect>,
    /// Per-zone effect, key `"{device_id}/{zone_id}"`; missing = `Firmware`.
    pub zones: BTreeMap<String, Effect>,
}

impl Default for RgbConfig {
    fn default() -> Self {
        Self { brightness: 100, sync: None, zones: BTreeMap::new() }
    }
}

impl RgbConfig {
    /// Normalizes the RGB settings (bounds) and rejects inconsistent effects.
    pub fn validate(&mut self) -> Result<(), crate::config::ConfigError> {
        self.brightness = self.brightness.min(100);
        if let Some(effect) = &mut self.sync {
            effect.validate().map_err(|e| invalid(format!("synced effect: {e}")))?;
        }
        for (zone, effect) in &mut self.zones {
            effect.validate().map_err(|e| invalid(format!("zone \"{zone}\": {e}")))?;
        }
        Ok(())
    }
}

fn invalid(msg: String) -> crate::config::ConfigError {
    crate::config::ConfigError::Invalid(msg)
}

impl Effect {
    /// Normalizes an effect; returns the rejection reason.
    fn validate(&mut self) -> Result<(), String> {
        match self {
            Self::Firmware | Self::Off | Self::Static { .. } => {}
            Self::Breathing { period_ms, .. } | Self::Rainbow { period_ms } => {
                *period_ms = (*period_ms).clamp(200, 60_000);
            }
            Self::Wave { colors, period_ms } => {
                if colors.is_empty() {
                    return Err("the wave must contain at least one color".into());
                }
                *period_ms = (*period_ms).clamp(200, 60_000);
            }
            Self::Temperature { source, min_temp, max_temp, .. } => {
                let ids = match source {
                    TempSource::Sensor { id } => std::slice::from_ref(&*id),
                    TempSource::Max { ids } | TempSource::Average { ids } => ids.as_slice(),
                };
                if ids.is_empty() || ids.iter().any(|id| id.trim().is_empty()) {
                    return Err("temperature source has no sensor".into());
                }
                if !(min_temp.is_finite() && max_temp.is_finite() && min_temp < max_temp) {
                    return Err("invalid temperature range (min < max required)".into());
                }
            }
            Self::Load { sensor, .. } => {
                if sensor.trim().is_empty() {
                    return Err("load sensor not set".into());
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ConfigError};

    fn validate_sync(effect: Effect) -> Result<RgbConfig, ConfigError> {
        let mut cfg = RgbConfig { sync: Some(effect), ..RgbConfig::default() };
        cfg.validate().map(|_| cfg)
    }

    fn assert_invalid(effect: Effect, needle: &str) {
        match validate_sync(effect) {
            Err(ConfigError::Invalid(msg)) => assert!(msg.contains(needle), "{msg}"),
            other => panic!("expected Invalid({needle}), got {other:?}"),
        }
    }

    fn temp(source: TempSource, min_temp: f32, max_temp: f32) -> Effect {
        Effect::Temperature { source, cold: Rgb::BLACK, hot: Rgb::WHITE, min_temp, max_temp }
    }

    #[test]
    fn validate_clamps() {
        let mut cfg = RgbConfig { brightness: 250, ..RgbConfig::default() };
        cfg.zones.insert("d/z1".into(), Effect::Rainbow { period_ms: 0 });
        cfg.zones.insert("d/z2".into(), Effect::Breathing { color: Rgb::WHITE, period_ms: 99_999 });
        cfg.zones.insert("d/z3".into(), Effect::Wave { colors: vec![Rgb::WHITE], period_ms: 10 });
        cfg.validate().expect("valid");
        assert_eq!(cfg.brightness, 100);
        assert_eq!(cfg.zones["d/z1"], Effect::Rainbow { period_ms: 200 });
        assert_eq!(cfg.zones["d/z2"], Effect::Breathing { color: Rgb::WHITE, period_ms: 60_000 });
        assert_eq!(cfg.zones["d/z3"], Effect::Wave { colors: vec![Rgb::WHITE], period_ms: 200 });
    }

    #[test]
    fn validate_rejects_bad_effects() {
        assert_invalid(Effect::Wave { colors: vec![], period_ms: 1000 }, "wave");
        let sensor = || TempSource::Sensor { id: "cpu/package_temp".into() };
        assert_invalid(temp(sensor(), 80.0, 40.0), "range");
        assert_invalid(temp(sensor(), 40.0, 40.0), "range");
        assert_invalid(temp(sensor(), f32::NAN, 40.0), "range");
        assert_invalid(temp(sensor(), 40.0, f32::INFINITY), "range");
        assert_invalid(temp(TempSource::Sensor { id: " ".into() }, 30.0, 80.0), "source");
        assert_invalid(temp(TempSource::Max { ids: vec![] }, 30.0, 80.0), "source");
        assert_invalid(Effect::Load { sensor: "".into(), low: Rgb::BLACK, high: Rgb::WHITE }, "load");
        assert!(validate_sync(temp(sensor(), 30.0, 80.0)).is_ok());

        let mut config = Config::default();
        config.rgb.zones.insert("d/jrgb1".into(), Effect::Wave { colors: vec![], period_ms: 1000 });
        match config.validate() {
            Err(ConfigError::Invalid(msg)) => assert!(msg.contains("d/jrgb1"), "{msg}"),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn rgb_config_roundtrips_through_toml() {
        let text = r#"
[rgb]
brightness = 80
sync = { effect = "rainbow", period_ms = 5000 }
"#;
        let config = Config::from_toml(text).expect("parse");
        assert_eq!(config.rgb.brightness, 80);
        assert_eq!(config.rgb.sync, Some(Effect::Rainbow { period_ms: 5000 }));

        let mut config = Config::default();
        config.rgb.sync = Some(Effect::Static { color: Rgb::new(1, 2, 3) });
        let zones = &mut config.rgb.zones;
        zones.insert("mystic-light/1462:7e06/jrgb1".into(), Effect::Off);
        zones.insert("d/z0".into(), Effect::Firmware);
        zones.insert("d/z1".into(), Effect::Wave { colors: vec![Rgb::WHITE, Rgb::new(255, 0, 0)], period_ms: 3000 });
        zones.insert("d/z2".into(), temp(TempSource::Average { ids: vec!["a".into(), "b".into()] }, 30.0, 80.0));
        zones.insert("d/z3".into(), Effect::Load { sensor: "cpu/load".into(), low: Rgb::BLACK, high: Rgb::WHITE });
        zones.insert("d/z4".into(), Effect::Breathing { color: Rgb::WHITE, period_ms: 2000 });
        config.validate().expect("valid");
        let text = config.to_toml().expect("serialization");
        assert_eq!(Config::from_toml(&text).expect("parse"), config, "{text}");
    }
}
