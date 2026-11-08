//! NVIDIA GPU via NVML (library loaded at runtime, shipped with the driver).

use super::bytes_to_mib;
use hub_core::fan::{FanChannel, FanController};
use hub_core::{Device, HwError, Reading, SensorKind, SensorProvider};
use nvml_wrapper::Nvml;
use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};
use nvml_wrapper::error::NvmlError;
use std::collections::BTreeSet;

pub struct NvidiaProvider {
    nvml: Nvml,
    /// Short names by GPU index, e.g. "RTX 5080".
    names: Vec<String>,
}

impl NvidiaProvider {
    pub fn open() -> Result<Self, HwError> {
        let unavailable =
            |err: nvml_wrapper::error::NvmlError| HwError::Unavailable(format!("NVML: {err}"));
        let nvml = Nvml::init().map_err(unavailable)?;
        let count = nvml.device_count().map_err(unavailable)?;
        let names = (0..count)
            .map(|i| {
                let name = nvml
                    .device_by_index(i)
                    .and_then(|gpu| gpu.name())
                    .unwrap_or_else(|_| format!("GPU {i}"));
                short_name(&name).to_owned()
            })
            .collect();
        Ok(Self { nvml, names })
    }
}

/// "NVIDIA GeForce RTX 5080" -> "RTX 5080".
fn short_name(name: &str) -> &str {
    name.trim_start_matches("NVIDIA ")
        .trim_start_matches("GeForce ")
}

impl SensorProvider for NvidiaProvider {
    fn name(&self) -> &str {
        "nvidia"
    }

    fn poll(&mut self, out: &mut Vec<Reading>) -> Result<(), HwError> {
        for (i, name) in self.names.iter().enumerate() {
            let Ok(gpu) = self.nvml.device_by_index(i as u32) else {
                continue;
            };
            // An unsupported metric (Err) is simply omitted.
            let mut push = |metric: &str, label: &str, kind: SensorKind, value: Option<f32>| {
                if let Some(value) = value {
                    out.push(Reading {
                        id: format!("gpu{i}/{metric}"),
                        label: format!("{name} {label}"),
                        device: Device::Gpu,
                        kind,
                        value,
                    });
                }
            };
            push(
                "temp",
                "Temp",
                SensorKind::Temperature,
                gpu.temperature(TemperatureSensor::Gpu)
                    .ok()
                    .map(|t| t as f32),
            );
            push(
                "load",
                "Load",
                SensorKind::Load,
                gpu.utilization_rates().ok().map(|u| u.gpu as f32),
            );
            push(
                "vram_used",
                "VRAM Used",
                SensorKind::Memory,
                gpu.memory_info().ok().map(|m| bytes_to_mib(m.used)),
            );
            push(
                "power",
                "Power",
                SensorKind::Power,
                gpu.power_usage().ok().map(|mw| mw as f32 / 1000.0),
            );
            push(
                "core_clock",
                "Core Clock",
                SensorKind::Clock,
                gpu.clock_info(Clock::Graphics).ok().map(|c| c as f32),
            );
            // NVML gives the fan target in % (not RPM): exposed as Load to keep the right unit.
            for j in 0..gpu.num_fans().unwrap_or(0) {
                push(
                    &format!("fan{j}"),
                    &format!("Fan {j}"),
                    SensorKind::Load,
                    gpu.fan_speed(j).ok().map(|f| f as f32),
                );
            }
        }
        Ok(())
    }
}

/// GPU fans driven via NVML (writing requires admin rights).
pub struct NvidiaFans {
    nvml: Nvml,
    channels: Vec<FanChannel>,
    /// (gpu, fan) channels switched to manual: only those are handed back to the driver.
    touched: BTreeSet<(u32, u32)>,
}

impl NvidiaFans {
    pub fn open() -> Result<Self, HwError> {
        let nvml = Nvml::init().map_err(map_err)?;
        let mut channels = Vec::new();
        for i in 0..nvml.device_count().map_err(map_err)? {
            let Ok(gpu) = nvml.device_by_index(i) else {
                continue;
            };
            let name = gpu.name().unwrap_or_else(|_| format!("GPU {i}"));
            for j in 0..gpu.num_fans().unwrap_or(0) {
                channels.push(FanChannel {
                    id: format!("gpu{i}/fan{j}"),
                    label: format!("{} Fan {}", short_name(&name), j + 1),
                    rpm_sensor: None,
                });
            }
        }
        Ok(Self {
            nvml,
            channels,
            touched: BTreeSet::new(),
        })
    }

    fn write(
        &self,
        channel: &str,
        action: impl FnOnce(&mut nvml_wrapper::Device<'_>, u32) -> Result<(), NvmlError>,
    ) -> Result<(u32, u32), HwError> {
        let (i, j) = parse_channel(channel)
            .ok_or_else(|| HwError::Other(format!("unknown channel: {channel}")))?;
        let mut gpu = self.nvml.device_by_index(i).map_err(map_err)?;
        action(&mut gpu, j).map_err(map_err)?;
        Ok((i, j))
    }
}

/// "gpu0/fan1" -> (0, 1).
fn parse_channel(id: &str) -> Option<(u32, u32)> {
    let (gpu, fan) = id.split_once('/')?;
    Some((
        gpu.strip_prefix("gpu")?.parse().ok()?,
        fan.strip_prefix("fan")?.parse().ok()?,
    ))
}

fn map_err(err: NvmlError) -> HwError {
    match err {
        NvmlError::NoPermission => HwError::PermissionDenied(format!("NVML: {err}")),
        other => HwError::Other(format!("NVML: {other}")),
    }
}

impl FanController for NvidiaFans {
    fn name(&self) -> &str {
        "nvidia"
    }

    fn channels(&self) -> &[FanChannel] {
        &self.channels
    }

    fn set_duty(&mut self, channel: &str, percent: u8) -> Result<(), HwError> {
        let percent = u32::from(percent.min(100));
        let key = self.write(channel, |gpu, j| gpu.set_fan_speed(j, percent))?;
        self.touched.insert(key);
        Ok(())
    }

    fn set_auto(&mut self, channel: &str) -> Result<(), HwError> {
        let key = self.write(channel, |gpu, j| gpu.set_default_fan_speed(j))?;
        self.touched.remove(&key);
        Ok(())
    }

    fn restore_all(&mut self) {
        for (i, j) in std::mem::take(&mut self.touched) {
            let result = self
                .nvml
                .device_by_index(i)
                .and_then(|mut gpu| gpu.set_default_fan_speed(j));
            if let Err(err) = result {
                tracing::error!("restoring gpu{i}/fan{j}: {err}");
            }
        }
    }
}

impl Drop for NvidiaFans {
    fn drop(&mut self) {
        self.restore_all();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parse_channel_ids() {
        assert_eq!(super::parse_channel("gpu0/fan1"), Some((0, 1)));
        assert_eq!(super::parse_channel("gpu0/temp"), None);
        assert_eq!(super::parse_channel("nct6687/pwm1"), None);
    }

    #[test]
    fn short_name_strips_vendor() {
        assert_eq!(super::short_name("NVIDIA GeForce RTX 5080"), "RTX 5080");
        assert_eq!(super::short_name("Tesla T4"), "Tesla T4");
    }
}
