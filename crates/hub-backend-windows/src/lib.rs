//! Backend Windows via le pilote PawnIO : température CPU Intel (module IntelMSR)
//! et Super I/O Nuvoton NCT6687D (module LpcIO).
#![cfg(windows)]

use std::path::Path;

use hub_core::{Device, HwError, Reading, SensorKind, SensorProvider};

mod nct6687;
mod pawnio;

pub use nct6687::{Nct6687Fans, Nct6687Sensors, open_nct6687};
use pawnio::PawnIo;

const MSR_TEMPERATURE_TARGET: u64 = 0x1A2;
const MSR_PACKAGE_THERM_STATUS: u64 = 0x1B1;

/// Température package d'un CPU Intel, lue par MSR via PawnIO.
pub struct IntelCpuTemp {
    pawn: PawnIo,
    tj_max: u64,
}

impl IntelCpuTemp {
    /// Ouvre `\\?\GLOBALROOT\Device\PawnIO` et charge le module signé IntelMSR depuis `module_path`.
    pub fn open(module_path: &Path) -> Result<Self, HwError> {
        if !is_genuine_intel() {
            return Err(HwError::Unavailable("CPU non Intel".into()));
        }
        let mut sensor = Self { pawn: PawnIo::open(module_path)?, tj_max: 0 };
        sensor.tj_max = (sensor.read_msr(MSR_TEMPERATURE_TARGET)? >> 16) & 0xFF;
        Ok(sensor)
    }

    /// Appelle `ioctl_read_msr` du module : entrée = n° MSR, sortie = valeur.
    fn read_msr(&self, msr: u64) -> Result<u64, HwError> {
        let mut value = [0u64];
        self.pawn.execute("ioctl_read_msr", &[msr], &mut value)?;
        Ok(value[0])
    }
}

impl SensorProvider for IntelCpuTemp {
    fn name(&self) -> &str {
        "intel-msr"
    }

    fn poll(&mut self, out: &mut Vec<Reading>) -> Result<(), HwError> {
        let status = self.read_msr(MSR_PACKAGE_THERM_STATUS)?;
        out.push(Reading {
            id: "cpu/package_temp".into(),
            label: "CPU Package".into(),
            device: Device::Cpu,
            kind: SensorKind::Temperature,
            value: package_temp(self.tj_max, status),
        });
        Ok(())
    }
}

/// Température = TjMax - offset numérique (bits 22:16 de IA32_PACKAGE_THERM_STATUS).
fn package_temp(tj_max: u64, therm_status: u64) -> f32 {
    tj_max.saturating_sub((therm_status >> 16) & 0x7F) as f32
}

fn is_genuine_intel() -> bool {
    let r = std::arch::x86_64::__cpuid(0);
    [r.ebx, r.edx, r.ecx]
        .iter()
        .flat_map(|reg| reg.to_le_bytes())
        .eq(*b"GenuineIntel")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_temp_from_raw_msr() {
        // TjMax 100 °C, offset 0x23 = 35 → 65 °C ; bits hors champ ignorés.
        assert_eq!(package_temp(100, 0x8823_0000 | 0xFFFF), 65.0);
        assert_eq!(package_temp(100, 0x7F << 16), 0.0);
    }
}
