//! Éclairage RGB natif (sans OpenRGB) : moteur d'effets + pilotes d'appareils.

pub mod effects;
pub mod mystic_light;

use hub_core::rgb::RgbDevice;

/// Ouvre tous les appareils RGB supportés présents (lecture seule à l'ouverture).
/// Un appareil absent ou en erreur est simplement ignoré (log `warn`).
pub fn detect() -> Vec<Box<dyn RgbDevice>> {
    mystic_light::detect()
}
