//! Contrat de l'éclairage RGB : appareils, zones, effets.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::HwError;
use crate::config::TempSource;

/// Couleur 24 bits.
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

/// Zone d'un appareil (en-tête JRGB, JARGB, logo…). `leds` = 1 si non adressable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RgbZone {
    /// Id stable, unique dans l'appareil, ex. `jargb1`.
    pub id: String,
    pub label: String,
    pub leds: usize,
}

/// Appareil RGB détecté.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RgbDeviceInfo {
    /// Id stable, ex. `mystic-light/1462:7e06`.
    pub id: String,
    pub label: String,
    pub zones: Vec<RgbZone>,
}

/// Pilote d'un appareil RGB.
///
/// Les implémentations n'écrivent rien à l'ouverture, sauvegardent l'état d'origine avant
/// la première écriture et appellent `restore` dans leur `Drop`.
pub trait RgbDevice: Send {
    fn info(&self) -> &RgbDeviceInfo;
    /// Applique une image complète : `frame[zone][led]`, dans l'ordre de `info().zones`.
    /// Appelé jusqu'à ~30 fois par seconde : l'implémentation ignore les images identiques.
    fn set_frame(&mut self, frame: &[Vec<Rgb>]) -> Result<(), HwError>;
    /// Rend l'éclairage au firmware (effet d'origine) ; ne doit jamais paniquer.
    fn restore(&mut self);
}

/// Effet appliqué à une zone (ou à tout, en mode synchronisé).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum Effect {
    /// Rendu au firmware : le daemon n'écrit rien.
    Firmware,
    Off,
    Static { color: Rgb },
    Breathing { color: Rgb, period_ms: u32 },
    Rainbow { period_ms: u32 },
    /// Dégradé qui défile le long des LEDs.
    Wave { colors: Vec<Rgb>, period_ms: u32 },
    /// Couleur interpolée entre `cold` (≤ `min_temp`) et `hot` (≥ `max_temp`).
    Temperature { source: TempSource, cold: Rgb, hot: Rgb, min_temp: f32, max_temp: f32 },
    /// Couleur selon une mesure de charge (%), ex. `cpu/load`.
    Load { sensor: String, low: Rgb, high: Rgb },
}

/// Réglages RGB, communs à tous les profils.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RgbConfig {
    /// Luminosité globale 0..=100 %.
    pub brightness: u8,
    /// `Some` = même effet partout (synchronisation globale).
    pub sync: Option<Effect>,
    /// Effet par zone, clé `"{device_id}/{zone_id}"` ; absent = `Firmware`.
    pub zones: BTreeMap<String, Effect>,
}

impl Default for RgbConfig {
    fn default() -> Self {
        Self { brightness: 100, sync: None, zones: BTreeMap::new() }
    }
}

impl RgbConfig {
    /// Normalise les réglages RGB (bornes) et refuse les effets incohérents.
    pub fn validate(&mut self) -> Result<(), crate::config::ConfigError> {
        self.brightness = self.brightness.min(100);
        if let Some(effect) = &mut self.sync {
            effect.validate().map_err(|e| invalid(format!("effet synchronisé : {e}")))?;
        }
        for (zone, effect) in &mut self.zones {
            effect.validate().map_err(|e| invalid(format!("zone « {zone} » : {e}")))?;
        }
        Ok(())
    }
}

fn invalid(msg: String) -> crate::config::ConfigError {
    crate::config::ConfigError::Invalid(msg)
}

impl Effect {
    /// Normalise un effet ; renvoie le motif du refus en français.
    fn validate(&mut self) -> Result<(), String> {
        match self {
            Self::Firmware | Self::Off | Self::Static { .. } => {}
            Self::Breathing { period_ms, .. } | Self::Rainbow { period_ms } => {
                *period_ms = (*period_ms).clamp(200, 60_000);
            }
            Self::Wave { colors, period_ms } => {
                if colors.is_empty() {
                    return Err("la vague doit contenir au moins une couleur".into());
                }
                *period_ms = (*period_ms).clamp(200, 60_000);
            }
            Self::Temperature { source, min_temp, max_temp, .. } => {
                let ids = match source {
                    TempSource::Sensor { id } => std::slice::from_ref(&*id),
                    TempSource::Max { ids } | TempSource::Average { ids } => ids.as_slice(),
                };
                if ids.is_empty() || ids.iter().any(|id| id.trim().is_empty()) {
                    return Err("source de température sans capteur".into());
                }
                if !(min_temp.is_finite() && max_temp.is_finite() && min_temp < max_temp) {
                    return Err("plage de température invalide (min < max requis)".into());
                }
            }
            Self::Load { sensor, .. } => {
                if sensor.trim().is_empty() {
                    return Err("capteur de charge non renseigné".into());
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
            other => panic!("attendu Invalid({needle}), obtenu {other:?}"),
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
        cfg.validate().expect("valide");
        assert_eq!(cfg.brightness, 100);
        assert_eq!(cfg.zones["d/z1"], Effect::Rainbow { period_ms: 200 });
        assert_eq!(cfg.zones["d/z2"], Effect::Breathing { color: Rgb::WHITE, period_ms: 60_000 });
        assert_eq!(cfg.zones["d/z3"], Effect::Wave { colors: vec![Rgb::WHITE], period_ms: 200 });
    }

    #[test]
    fn validate_rejects_bad_effects() {
        assert_invalid(Effect::Wave { colors: vec![], period_ms: 1000 }, "vague");
        let sensor = || TempSource::Sensor { id: "cpu/package_temp".into() };
        assert_invalid(temp(sensor(), 80.0, 40.0), "plage");
        assert_invalid(temp(sensor(), 40.0, 40.0), "plage");
        assert_invalid(temp(sensor(), f32::NAN, 40.0), "plage");
        assert_invalid(temp(sensor(), 40.0, f32::INFINITY), "plage");
        assert_invalid(temp(TempSource::Sensor { id: " ".into() }, 30.0, 80.0), "source");
        assert_invalid(temp(TempSource::Max { ids: vec![] }, 30.0, 80.0), "source");
        assert_invalid(Effect::Load { sensor: "".into(), low: Rgb::BLACK, high: Rgb::WHITE }, "charge");
        assert!(validate_sync(temp(sensor(), 30.0, 80.0)).is_ok());

        let mut config = Config::default();
        config.rgb.zones.insert("d/jrgb1".into(), Effect::Wave { colors: vec![], period_ms: 1000 });
        match config.validate() {
            Err(ConfigError::Invalid(msg)) => assert!(msg.contains("d/jrgb1"), "{msg}"),
            other => panic!("attendu Invalid, obtenu {other:?}"),
        }
    }

    #[test]
    fn rgb_config_roundtrips_through_toml() {
        let text = r#"
[rgb]
brightness = 80
sync = { effect = "rainbow", period_ms = 5000 }
"#;
        let config = Config::from_toml(text).expect("lecture");
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
        config.validate().expect("valide");
        let text = config.to_toml().expect("sérialisation");
        assert_eq!(Config::from_toml(&text).expect("lecture"), config, "{text}");
    }
}
