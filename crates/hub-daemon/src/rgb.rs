//! Thread RGB : possède tous les appareils RGB et rend les effets à ~30 images/s.
//!
//! Règle : un appareil est soit entièrement piloté par le logiciel, soit entièrement rendu au
//! firmware (la plupart des contrôleurs ne savent pas laisser une seule zone au firmware).
//! Si au moins une zone a un effet logiciel, les zones `Firmware` du même appareil sont éteintes.
//!
//! Sécurité : la `RgbConfig` par défaut n'a ni `sync` ni zone -> tout est `Firmware` -> rien
//! n'est jamais écrit tant que l'utilisateur n'a pas configuré d'effet.

use hub_core::Reading;
use hub_core::ipc::{self, DaemonMsg};
use hub_core::rgb::{Rgb, RgbConfig, RgbDevice, RgbDeviceInfo, RgbZone};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, watch};

/// ~30 images/s.
const FRAME: Duration = Duration::from_millis(33);

/// Messages reçus par le thread RGB.
pub enum RgbCmd {
    /// Nouvelle config RGB (envoyée par le thread de contrôle quand elle change).
    Config(RgbConfig),
    /// Dernières mesures (à chaque tick du thread de contrôle), pour les effets température/charge.
    Readings(Vec<Reading>),
    Stop,
}

/// Image complète d'un appareil, ou `None` si toutes ses zones sont `Firmware`.
/// `rendered[i]` = rendu de la zone `i` (`None` = Firmware) ; une zone Firmware d'un appareil
/// piloté est éteinte.
fn assemble(zones: &[RgbZone], rendered: Vec<Option<Vec<Rgb>>>) -> Option<Vec<Vec<Rgb>>> {
    if rendered.iter().all(Option::is_none) {
        return None;
    }
    Some(
        zones
            .iter()
            .zip(rendered)
            .map(|(zone, leds)| leds.unwrap_or_else(|| vec![Rgb::BLACK; zone.leds]))
            .collect(),
    )
}

/// Un appareil et son état de pilotage.
struct Driven {
    device: Box<dyn RgbDevice>,
    /// Vrai si le logiciel a écrit depuis le dernier `restore`.
    software: bool,
    last_error: Option<String>,
}

impl Driven {
    fn render(&mut self, config: &RgbConfig, readings: &[Reading], elapsed: Duration) {
        let info = self.device.info();
        let rendered = info
            .zones
            .iter()
            .map(|zone| {
                let effect = hub_rgb::effects::effect_for(config, &info.id, &zone.id);
                hub_rgb::effects::render(effect, zone.leds, elapsed, readings, config.brightness)
            })
            .collect();
        match assemble(&info.zones, rendered) {
            None => {
                if self.software {
                    self.device.restore();
                    self.software = false;
                    self.last_error = None;
                }
            }
            Some(frame) => {
                self.software = true;
                let error = self.device.set_frame(&frame).err().map(|e| e.to_string());
                if let Some(message) = &error
                    && error != self.last_error
                {
                    tracing::warn!("RGB {} : {message}", self.device.info().id);
                }
                self.last_error = error;
            }
        }
    }
}

/// Publie la liste des appareils : gardée pour les nouveaux clients et diffusée aux connectés.
fn publish_devices(
    devices: Vec<RgbDeviceInfo>,
    live: &broadcast::Sender<String>,
    state: &watch::Sender<String>,
) {
    match ipc::encode(&DaemonMsg::Rgb { devices }) {
        Ok(line) => {
            state.send_replace(line.clone());
            let _ = live.send(line); // aucun client : rien à faire
        }
        Err(err) => tracing::error!("encodage RGB : {err}"),
    }
}

/// Tourne jusqu'à `Stop` (ou fermeture du canal). Les appareils sont ouverts ici et lâchés à la
/// fin : leur `Drop` rend l'éclairage au firmware.
pub fn run(
    mut config: RgbConfig,
    commands: Receiver<RgbCmd>,
    live: broadcast::Sender<String>,
    state: watch::Sender<String>,
) {
    let mut devices: Vec<Driven> = hub_rgb::detect()
        .into_iter()
        .map(|device| Driven {
            device,
            software: false,
            last_error: None,
        })
        .collect();
    tracing::info!("{} appareil(s) RGB", devices.len());
    publish_devices(
        devices.iter().map(|d| d.device.info().clone()).collect(),
        &live,
        &state,
    );

    if devices.is_empty() {
        // Rien à piloter : on dort sur le canal jusqu'à l'arrêt (pas de boucle active).
        while let Ok(cmd) = commands.recv() {
            if matches!(cmd, RgbCmd::Stop) {
                break;
            }
        }
        return;
    }

    let mut readings = Vec::new();
    let start = Instant::now();
    let mut next_frame = start;
    loop {
        match commands.recv_timeout(next_frame.saturating_duration_since(Instant::now())) {
            Ok(RgbCmd::Config(new)) => config = new,
            Ok(RgbCmd::Readings(new)) => readings = new,
            Ok(RgbCmd::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                let elapsed = start.elapsed();
                for device in &mut devices {
                    device.render(&config, &readings, elapsed);
                }
                // Rattrape sans rafale si une image a pris du retard.
                next_frame = (next_frame + FRAME).max(Instant::now());
            }
        }
    }
    tracing::info!("arrêt : éclairage rendu au firmware");
    // `devices` lâché ici -> Drop -> restore.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone(id: &str, leds: usize) -> RgbZone {
        RgbZone {
            id: id.into(),
            label: id.into(),
            leds,
        }
    }

    #[test]
    fn all_firmware_writes_nothing() {
        let zones = [zone("jrgb1", 1), zone("jargb1", 3)];
        assert_eq!(assemble(&zones, vec![None, None]), None);
        assert_eq!(assemble(&[], Vec::new()), None);
    }

    #[test]
    fn firmware_zone_turns_off_in_driven_device() {
        let zones = [zone("jrgb1", 1), zone("jargb1", 3)];
        let red = Rgb::new(255, 0, 0);
        assert_eq!(
            assemble(&zones, vec![Some(vec![red]), None]),
            Some(vec![vec![red], vec![Rgb::BLACK; 3]])
        );
    }

    #[test]
    fn default_config_is_all_firmware() {
        let config = RgbConfig::default();
        assert!(config.sync.is_none() && config.zones.is_empty());
    }
}
