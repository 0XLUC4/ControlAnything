//! RGB thread: owns all RGB devices and renders effects at ~30 frames/s.
//!
//! Rule: a device is either fully driven by software or fully handed back to firmware
//! (most controllers cannot leave a single zone to the firmware).
//! If at least one zone has a software effect, the `Firmware` zones of the same device are turned off.
//!
//! Safety: the default `RgbConfig` has neither `sync` nor zones -> everything is `Firmware` -> nothing
//! is ever written until the user has configured an effect.

use hub_core::Reading;
use hub_core::ipc::{self, DaemonMsg};
use hub_core::rgb::{Rgb, RgbConfig, RgbDevice, RgbDeviceInfo, RgbZone};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, watch};

/// ~30 frames/s.
const FRAME: Duration = Duration::from_millis(33);

/// Messages received by the RGB thread.
pub enum RgbCmd {
    /// New RGB config (sent by the control thread when it changes).
    Config(RgbConfig),
    /// Latest readings (on each control thread tick), for temperature/load effects.
    Readings(Vec<Reading>),
    Stop,
}

/// Full frame of a device, or `None` if all its zones are `Firmware`.
/// `rendered[i]` = render of zone `i` (`None` = Firmware); a Firmware zone of a driven
/// device is turned off.
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

/// A device and its driving state.
struct Driven {
    device: Box<dyn RgbDevice>,
    /// True if software has written since the last `restore`.
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
                    tracing::warn!("RGB {}: {message}", self.device.info().id);
                }
                self.last_error = error;
            }
        }
    }
}

/// Publishes the device list: kept for new clients and broadcast to connected ones.
fn publish_devices(
    devices: Vec<RgbDeviceInfo>,
    live: &broadcast::Sender<String>,
    state: &watch::Sender<String>,
) {
    match ipc::encode(&DaemonMsg::Rgb { devices }) {
        Ok(line) => {
            state.send_replace(line.clone());
            let _ = live.send(line); // no client: nothing to do
        }
        Err(err) => tracing::error!("encoding RGB: {err}"),
    }
}

/// Runs until `Stop` (or channel closure). Devices are opened here and dropped at the
/// end: their `Drop` hands the lighting back to firmware.
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
    tracing::info!("{} RGB device(s)", devices.len());
    publish_devices(
        devices.iter().map(|d| d.device.info().clone()).collect(),
        &live,
        &state,
    );

    if devices.is_empty() {
        // Nothing to drive: sleep on the channel until stop (no busy loop).
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
                // Catch up without a burst if a frame ran late.
                next_frame = (next_frame + FRAME).max(Instant::now());
            }
        }
    }
    tracing::info!("stopping: lighting handed back to firmware");
    // `devices` dropped here -> Drop -> restore.
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
