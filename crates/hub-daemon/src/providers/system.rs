//! Charge CPU et mémoire via sysinfo (multi-plateforme, sans droits).

use super::bytes_to_mib;
use hub_core::{Device, HwError, Reading, SensorKind, SensorProvider};
use sysinfo::System;

pub struct SysinfoProvider {
    system: System,
}

impl SysinfoProvider {
    pub fn new() -> Result<Self, HwError> {
        let system =
            System::new().map_err(|err| HwError::Unavailable(format!("sysinfo : {err}")))?;
        Ok(Self { system })
    }
}

fn reading(id: &str, label: &str, device: Device, kind: SensorKind, value: f32) -> Reading {
    Reading {
        id: id.into(),
        label: label.into(),
        device,
        kind,
        value,
    }
}

impl SensorProvider for SysinfoProvider {
    fn name(&self) -> &str {
        "sysinfo"
    }

    fn poll(&mut self, out: &mut Vec<Reading>) -> Result<(), HwError> {
        // La charge CPU est un delta entre deux refresh : le 1er poll renvoie 0 %.
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        let used = self.system.used_memory();
        let total = self.system.total_memory();
        out.push(reading(
            "cpu/load",
            "CPU Load",
            Device::Cpu,
            SensorKind::Load,
            self.system.global_cpu_usage(),
        ));
        out.push(reading(
            "ram/used",
            "RAM Used",
            Device::Memory,
            SensorKind::Memory,
            bytes_to_mib(used),
        ));
        out.push(reading(
            "ram/total",
            "RAM Total",
            Device::Memory,
            SensorKind::Memory,
            bytes_to_mib(total),
        ));
        if total > 0 {
            let load = (used as f64 / total as f64 * 100.0) as f32;
            out.push(reading(
                "ram/load",
                "RAM Load",
                Device::Memory,
                SensorKind::Load,
                load,
            ));
        }
        Ok(())
    }
}
