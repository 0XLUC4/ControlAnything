//! Nuvoton NCT6687D Super I/O (MSI Intel 600/700, AMD 500/600 boards) via the PawnIO LpcIO module.
//!
//! Register map derived from LibreHardwareMonitor (MPL-2.0): this file is under MPL-2.0.
//! This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.
//! If a copy of the MPL was not distributed with this file, You can obtain one at
//! http://mozilla.org/MPL/2.0/.
//!
//! References (LibreHardwareMonitor master, Nov. 2026, `LibreHardwareMonitorLib/`):
//! - `Hardware/Motherboard/Lpc/LpcIO.cs`: detection (ID 0xD5/0x92 l.409-428, flow l.466-500,
//!   `IsInvalidRuntimeBase` l.755, constants l.780-805);
//! - `Hardware/Motherboard/Lpc/LpcPort.cs`: config enter/exit keys (l.66-75);
//! - `Hardware/Motherboard/Lpc/Nct677X.cs`: EC space (l.28-31, access l.1259-1347),
//!   NCT6687D registers (l.101-108, 527-540, 573), temp. decoding (l.880-886) and RPM (l.1052-1066),
//!   control (l.745-766), save/restore (l.1458-1540);
//! - `Hardware/Motherboard/SuperIOHardware.cs`: % -> byte (l.120-125), MSI labels (l.607-660).
//!
//! Cross-checked with the Linux driver github.com/Fred78290/nct6687d (`nct6687.c`, "default" mapping,
//! which that driver explicitly reserves for non-DR NCT6687D, Z790 included).
//! NOT TESTED on real hardware at the time of writing.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::sleep;
use std::time::{Duration, Instant};

use hub_core::fan::{FanChannel, FanController};
use hub_core::{Device, HwError, Reading, SensorKind, SensorProvider};
use windows::Win32::Foundation::{HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::w;

use crate::pawnio::PawnIo;

// --- Super I/O configuration space (LpcIO.cs, LpcPort.cs), high confidence ---
/// (PawnIO slot, index port): slot 0 = 0x2E/0x2F, slot 1 = 0x4E/0x4F.
const SLOTS: [(u64, u16); 2] = [(0, 0x2E), (1, 0x4E)];
const ENTER_KEY: u8 = 0x87; // written twice (Winbond/Nuvoton/Fintek)
const EXIT_KEY: u8 = 0xAA;
const CHIP_ID_REGISTER: u8 = 0x20;
const CHIP_REVISION_REGISTER: u8 = 0x21;
const DEVICE_SELECT_REGISTER: u8 = 0x07;
const HWM_LOGICAL_DEVICE: u8 = 0x0B;
const BASE_ADDRESS_REGISTER: u8 = 0x60;
/// (ID, revision) of the NCT6687D (and of the NCT6687DR, told apart by board name).
const NCT6687D_ID: (u8, u8) = (0xD5, 0x92);
/// MSI boards with the DR variant, different control registers (LpcIO.cs l.413-418).
const NCT6687DR_BOARDS: [&str; 5] = ["B840", "B850", "B860", "X870", "Z890"];

// --- EC access by page/index/data from the BAR (Nct677X.cs l.28-31, 1259-1347), high confidence ---
const EC_PAGE_OFFSET: u16 = 0x04;
const EC_INDEX_OFFSET: u16 = 0x05;
const EC_DATA_OFFSET: u16 = 0x06;
/// Page register value when the EC is free; rewritten after each access.
const EC_PAGE_FREE: u8 = 0xFF;
const EC_ACCESS_TIMEOUT: Duration = Duration::from_millis(500);

// --- NCT6687D EC registers (Nct677X.cs l.101-108, 527-540, 573) ---
/// Temperatures: signed integer at `0x100 + 2n`, half degree in bit 7 of the next byte. High confidence.
const TEMP_BASE: u16 = 0x100;
/// RPM: 16-bit big-endian at `0x140 + 2n` ("default" mapping, 8 channels). High confidence.
const FAN_RPM_BASE: u16 = 0x140;
/// Shared mode: bit n = channel n in manual. Medium confidence (LHM + Linux agree).
const FAN_CONTROL_MODE: u16 = 0xA00;
/// Configuration request: 0x80 = start, 0x40 = end. Medium confidence.
const FAN_PWM_REQUEST: u16 = 0xA01;
const FAN_CFG_REQUEST: u8 = 0x80;
const FAN_CFG_COMPLETE: u8 = 0x40;
/// Manual PWM setpoint (0..=255) of channel n at `0xA28 + n`. Medium confidence.
const FAN_PWM_COMMAND_BASE: u16 = 0xA28;
/// Pause after each config request, like LHM (`Thread.Sleep(50)`).
const FAN_CFG_DELAY: Duration = Duration::from_millis(50);

/// LHM default MSI labels (SuperIOHardware.cs l.607-660); index n -> `temp{n+1}`.
const TEMP_LABELS: [&str; 7] = ["CPU", "System", "VRM MOS", "PCH", "CPU Socket", "PCIe x1", "M2_1"];
/// LHM default MSI labels; index n -> `fan{n+1}` / `pwm{n+1}`.
const FAN_LABELS: [&str; 8] = [
    "CPU Fan",
    "Pump Fan",
    "System Fan 1",
    "System Fan 2",
    "System Fan 3",
    "System Fan 4",
    "System Fan 5",
    "System Fan 6",
];

/// Max wait for the cross-tool mutex (LHM, HWiNFO, MSI Center...) in normal operation.
const ISA_TIMEOUT: Duration = Duration::from_millis(100);
/// When restoring, wait longer, then restore anyway.
const ISA_RESTORE_TIMEOUT: Duration = Duration::from_millis(500);

/// Detects the NCT6687D via the PawnIO LpcIO module and returns two views sharing the chip.
///
/// Read-only on the hardware monitor side: only protocol writes are made
/// (config mode enter/exit keys, logical device selection, EC page register).
pub fn open_nct6687(lpcio_module: &Path) -> Result<(Nct6687Sensors, Nct6687Fans), HwError> {
    let product = baseboard_product()?;
    if is_nct6687dr_board(&product) {
        return Err(HwError::Unavailable(format!("board {product}: NCT6687DR not supported")));
    }
    let mut chip = Chip { pawn: PawnIo::open(lpcio_module)?, base: 0, isa_mutex: create_isa_mutex()? };
    let base = {
        let _isa = chip.lock_isa(ISA_TIMEOUT)?;
        chip.detect()?
    };
    chip.base = base;

    let chip = Arc::new(Mutex::new(chip));
    let channels = FAN_LABELS
        .iter()
        .enumerate()
        .map(|(index, label)| FanChannel {
            id: format!("nct6687/pwm{}", index + 1),
            label: (*label).into(),
            rpm_sensor: Some(format!("nct6687/fan{}", index + 1)),
        })
        .collect();
    let fans = Nct6687Fans { chip: Arc::clone(&chip), channels, saved: [None; FAN_LABELS.len()] };
    Ok((Nct6687Sensors { chip }, fans))
}

/// NCT6687D temperatures and RPM.
pub struct Nct6687Sensors {
    chip: Arc<Mutex<Chip>>,
}

impl SensorProvider for Nct6687Sensors {
    fn name(&self) -> &str {
        "nct6687"
    }

    fn poll(&mut self, out: &mut Vec<Reading>) -> Result<(), HwError> {
        let chip = lock(&self.chip);
        let _isa = chip.lock_isa(ISA_TIMEOUT)?;
        for (index, label) in TEMP_LABELS.iter().enumerate() {
            let register = temp_register(index);
            let value = decode_temperature(chip.ec_read(register)?, chip.ec_read(register + 1)?);
            out.push(reading(format!("nct6687/temp{}", index + 1), label, SensorKind::Temperature, value));
        }
        for (index, label) in FAN_LABELS.iter().enumerate() {
            let register = fan_rpm_register(index);
            let rpm = u16::from_be_bytes([chip.ec_read(register)?, chip.ec_read(register + 1)?]);
            out.push(reading(format!("nct6687/fan{}", index + 1), label, SensorKind::Fan, f32::from(rpm)));
        }
        Ok(())
    }
}

/// Original state of a channel, read just before the first write.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SavedChannel {
    /// Channel bit in `FAN_CONTROL_MODE` (0 = firmware auto).
    mode_bit: u8,
    command: u8,
}

/// PWM control of the 8 headers. Drop hands every modified channel back to the firmware.
pub struct Nct6687Fans {
    chip: Arc<Mutex<Chip>>,
    channels: Vec<FanChannel>,
    saved: [Option<SavedChannel>; FAN_LABELS.len()],
}

impl Nct6687Fans {
    fn index_of(&self, channel: &str) -> Result<usize, HwError> {
        self.channels
            .iter()
            .position(|c| c.id == channel)
            .ok_or_else(|| HwError::Other(format!("unknown channel: {channel}")))
    }
}

impl FanController for Nct6687Fans {
    fn name(&self) -> &str {
        "nct6687"
    }

    fn channels(&self) -> &[FanChannel] {
        &self.channels
    }

    /// LHM non-DR sequence (Nct677X.cs l.745-766): request, manual bit, setpoint, end.
    fn set_duty(&mut self, channel: &str, percent: u8) -> Result<(), HwError> {
        let index = self.index_of(channel)?;
        let chip = lock(&self.chip);
        let _isa = chip.lock_isa(ISA_TIMEOUT)?;
        let bit = mode_bit(index);
        if self.saved[index].is_none() {
            self.saved[index] = Some(SavedChannel {
                mode_bit: chip.ec_read(FAN_CONTROL_MODE)? & bit,
                command: chip.ec_read(command_register(index))?,
            });
        }
        chip.config_update(|| {
            let mode = chip.ec_read(FAN_CONTROL_MODE)?;
            chip.ec_write(FAN_CONTROL_MODE, mode | bit)?;
            chip.ec_write(command_register(index), duty_to_raw(percent))
        })
    }

    fn set_auto(&mut self, channel: &str) -> Result<(), HwError> {
        let index = self.index_of(channel)?;
        let Some(saved) = self.saved[index] else {
            return Ok(()); // never touched: already on firmware
        };
        let chip = lock(&self.chip);
        let _isa = chip.lock_isa(ISA_TIMEOUT)?;
        restore_channel(&chip, index, saved)?;
        self.saved[index] = None;
        Ok(())
    }

    fn restore_all(&mut self) {
        let chip = lock(&self.chip);
        // shortcut: if another tool holds the mutex > 500 ms we restore without it; fans come first.
        let _isa = chip.lock_isa(ISA_RESTORE_TIMEOUT).ok();
        for (index, slot) in self.saved.iter_mut().enumerate() {
            if let Some(saved) = *slot
                && restore_channel(&chip, index, saved).is_ok()
            {
                *slot = None;
            }
        }
    }
}

impl Drop for Nct6687Fans {
    fn drop(&mut self) {
        self.restore_all();
    }
}

/// LHM non-DR restore (Nct677X.cs l.1522-1535): mode bit first, then setpoint
/// in a config phase. The order (mode before request) is LHM's, kept as-is.
fn restore_channel(chip: &Chip, index: usize, saved: SavedChannel) -> Result<(), HwError> {
    let mode = chip.ec_read(FAN_CONTROL_MODE)?;
    chip.ec_write(FAN_CONTROL_MODE, restored_mode(mode, mode_bit(index), saved.mode_bit))?;
    chip.config_update(|| chip.ec_write(command_register(index), saved.command))
}

/// Detected chip: loaded LpcIO module + hardware monitor base address (BAR).
struct Chip {
    pawn: PawnIo,
    base: u16,
    isa_mutex: OwnedHandle,
}

impl Chip {
    /// Enters config mode on each slot, checks the exact ID, reads the BAR, exits.
    fn detect(&self) -> Result<u16, HwError> {
        for (slot, port) in SLOTS {
            self.pawn.execute("ioctl_select_slot", &[slot], &mut [])?;
            self.pio_out(port, ENTER_KEY)?;
            self.pio_out(port, ENTER_KEY)?;
            let id = (self.sio_in(CHIP_ID_REGISTER)?, self.sio_in(CHIP_REVISION_REGISTER)?);
            if id != NCT6687D_ID {
                // Like LHM: only exit if a chip answered.
                if !matches!(id.0, 0x00 | 0xFF) {
                    self.pio_out(port, EXIT_KEY)?;
                }
                continue;
            }
            let base = self.read_hwm_base();
            let exited = self.pio_out(port, EXIT_KEY);
            let base = base?;
            exited?;
            return Ok(base);
        }
        Err(HwError::Unavailable("NCT6687D not found at 0x2E/0x4E".into()))
    }

    /// `ioctl_find_bars` then allows access to the BAR ports (LpcIO.p).
    fn read_hwm_base(&self) -> Result<u16, HwError> {
        self.pawn.execute("ioctl_find_bars", &[], &mut [])?;
        self.sio_out(DEVICE_SELECT_REGISTER, HWM_LOGICAL_DEVICE)?;
        let address = self.sio_inw(BASE_ADDRESS_REGISTER)?;
        sleep(Duration::from_millis(1));
        let verify = self.sio_inw(BASE_ADDRESS_REGISTER)?;
        validate_base(address, verify)
    }

    /// Reads one byte of the EC space (address = page << 8 | index).
    fn ec_read(&self, address: u16) -> Result<u8, HwError> {
        self.ec_select(address)?;
        let value = self.pio_in(self.base + EC_DATA_OFFSET);
        let released = self.pio_out(self.base + EC_PAGE_OFFSET, EC_PAGE_FREE);
        let value = value?;
        released?;
        Ok(value)
    }

    fn ec_write(&self, address: u16, value: u8) -> Result<(), HwError> {
        self.ec_select(address)?;
        let written = self.pio_out(self.base + EC_DATA_OFFSET, value);
        let released = self.pio_out(self.base + EC_PAGE_OFFSET, EC_PAGE_FREE);
        written?;
        released
    }

    /// Waits for the EC to be free (page = 0xFF, 500 ms max then forces, like LHM) and points to `address`.
    fn ec_select(&self, address: u16) -> Result<(), HwError> {
        let page_port = self.base + EC_PAGE_OFFSET;
        let start = Instant::now();
        while self.pio_in(page_port)? != EC_PAGE_FREE {
            if start.elapsed() >= EC_ACCESS_TIMEOUT {
                self.pio_out(page_port, EC_PAGE_FREE)?;
                break;
            }
            sleep(Duration::from_millis(1));
        }
        let (page, index) = ec_page_index(address);
        self.pio_out(page_port, page)?;
        self.pio_out(self.base + EC_INDEX_OFFSET, index)
    }

    /// Wraps `body` in config request/end; the end is sent even if `body` fails,
    /// so the EC is never left stuck in config phase.
    fn config_update(&self, body: impl FnOnce() -> Result<(), HwError>) -> Result<(), HwError> {
        self.ec_write(FAN_PWM_REQUEST, FAN_CFG_REQUEST)?;
        sleep(FAN_CFG_DELAY);
        let applied = body();
        let completed = self.ec_write(FAN_PWM_REQUEST, FAN_CFG_COMPLETE);
        sleep(FAN_CFG_DELAY);
        applied?;
        completed
    }

    fn pio_in(&self, port: u16) -> Result<u8, HwError> {
        let mut out = [0u64];
        self.pawn.execute("ioctl_pio_inb", &[u64::from(port)], &mut out)?;
        Ok(out[0] as u8)
    }

    fn pio_out(&self, port: u16, value: u8) -> Result<(), HwError> {
        self.pawn.execute("ioctl_pio_outb", &[u64::from(port), u64::from(value)], &mut [])
    }

    fn sio_in(&self, register: u8) -> Result<u8, HwError> {
        let mut out = [0u64];
        self.pawn.execute("ioctl_superio_inb", &[u64::from(register)], &mut out)?;
        Ok(out[0] as u8)
    }

    fn sio_inw(&self, register: u8) -> Result<u16, HwError> {
        let mut out = [0u64];
        self.pawn.execute("ioctl_superio_inw", &[u64::from(register)], &mut out)?;
        Ok(out[0] as u16)
    }

    fn sio_out(&self, register: u8, value: u8) -> Result<(), HwError> {
        self.pawn.execute("ioctl_superio_outb", &[u64::from(register), u64::from(value)], &mut [])
    }

    /// `Global\Access_ISABUS.HTP.Method` mutex shared with LHM/HWiNFO/etc. (LpcIO.cs l.23).
    fn lock_isa(&self, timeout: Duration) -> Result<IsaGuard<'_>, HwError> {
        let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: valid mutex handle, owned by `self` which outlives the guard.
        let wait = unsafe { WaitForSingleObject(HANDLE(self.isa_mutex.as_raw_handle()), millis) };
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            Ok(IsaGuard(&self.isa_mutex))
        } else {
            Err(HwError::Other("ISA bus busy with another tool".into()))
        }
    }
}

/// Releases the ISA mutex on Drop (same thread as acquisition: local guard).
struct IsaGuard<'a>(&'a OwnedHandle);

impl Drop for IsaGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: mutex acquired by this thread in `lock_isa`, handle still open.
        let _ = unsafe { ReleaseMutex(HANDLE(self.0.as_raw_handle())) };
    }
}

fn create_isa_mutex() -> Result<OwnedHandle, HwError> {
    // SAFETY: static null-terminated name, default security attributes.
    let raw = unsafe { CreateMutexW(None, false, w!(r"Global\Access_ISABUS.HTP.Method")) }
        .map_err(|err| HwError::Io(err.into()))?;
    // SAFETY: valid handle from CreateMutexW, exclusively owned here.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw.0) })
}

/// Motherboard name (SMBIOS, copied by Windows into the registry).
fn baseboard_product() -> Result<String, HwError> {
    let mut buffer = [0u16; 256];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: buffer and size valid during the call, static null-terminated strings.
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"HARDWARE\DESCRIPTION\System\BIOS"),
            w!("BaseBoardProduct"),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    }
    .ok()
    .map_err(|err| HwError::Unavailable(format!("unreadable motherboard model: {err}")))?;
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Ok(String::from_utf16_lossy(&buffer[..len]))
}

fn lock(chip: &Mutex<Chip>) -> MutexGuard<'_, Chip> {
    // The chip has no Rust invariant a panic can break: recover the lock.
    chip.lock().unwrap_or_else(PoisonError::into_inner)
}

fn reading(id: String, label: &str, kind: SensorKind, value: f32) -> Reading {
    Reading { id, label: label.into(), device: Device::Motherboard, kind, value }
}

// --- Pure functions (tested) ---

fn is_nct6687dr_board(product: &str) -> bool {
    let product = product.to_ascii_uppercase();
    NCT6687DR_BOARDS.iter().any(|model| product.contains(model))
}

/// BAR stable across two reads, >= 0x100 and 8-aligned (LpcIO.cs l.755).
fn validate_base(address: u16, verify: u16) -> Result<u16, HwError> {
    if address != verify {
        return Err(HwError::Unavailable(format!("unstable BAR: {address:#x} then {verify:#x}")));
    }
    if address < 0x100 || address & 0xF007 != 0 {
        return Err(HwError::Unavailable(format!("invalid BAR: {address:#x}")));
    }
    Ok(address)
}

fn ec_page_index(address: u16) -> (u8, u8) {
    let [page, index] = address.to_be_bytes();
    (page, index)
}

fn temp_register(index: usize) -> u16 {
    TEMP_BASE + 2 * index as u16
}

fn fan_rpm_register(index: usize) -> u16 {
    FAN_RPM_BASE + 2 * index as u16
}

fn command_register(index: usize) -> u16 {
    FAN_PWM_COMMAND_BASE + index as u16
}

fn mode_bit(index: usize) -> u8 {
    1 << index
}

/// Signed byte + half degree in bit 7 of the next byte (Nct677X.cs l.884-886).
fn decode_temperature(integer: u8, half: u8) -> f32 {
    f32::from(integer as i8) + 0.5 * f32::from(half >> 7)
}

/// 0..=100 % -> 0..=255, f32 truncation like LHM's `GetSoftwareValueAsByte`.
fn duty_to_raw(percent: u8) -> u8 {
    (f32::from(percent.min(100)) * 2.55) as u8
}

/// Resets only the channel's bit to its saved value, leaving other channels untouched.
fn restored_mode(current: u8, bit: u8, saved_bit: u8) -> u8 {
    (current & !bit) | (saved_bit & bit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_are_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Nct6687Sensors>();
        assert_send::<Nct6687Fans>();
    }

    #[test]
    fn register_addresses() {
        assert_eq!(temp_register(0), 0x100);
        assert_eq!(temp_register(6), 0x10C);
        assert_eq!(fan_rpm_register(0), 0x140);
        assert_eq!(fan_rpm_register(7), 0x14E);
        assert_eq!(command_register(0), 0xA28);
        assert_eq!(command_register(7), 0xA2F);
        assert_eq!(mode_bit(7), 0x80);
        assert_eq!(ec_page_index(0xA28), (0x0A, 0x28));
        assert_eq!(ec_page_index(0x100), (0x01, 0x00));
    }

    #[test]
    fn duty_scaling_matches_lhm() {
        assert_eq!(duty_to_raw(0), 0);
        assert_eq!(duty_to_raw(1), 2); // 2.55 truncated
        assert_eq!(duty_to_raw(50), 127);
        assert_eq!(duty_to_raw(100), 255);
        assert_eq!(duty_to_raw(250), 255); // clamped to 100 %
    }

    #[test]
    fn temperature_decoding() {
        assert_eq!(decode_temperature(45, 0x00), 45.0);
        assert_eq!(decode_temperature(45, 0x80), 45.5);
        assert_eq!(decode_temperature(45, 0x7F), 45.0); // only bit 7 counts
        assert_eq!(decode_temperature(0xFE, 0x80), -1.5);
    }

    #[test]
    fn chip_and_board_checks() {
        assert_eq!(NCT6687D_ID, (0xD5, 0x92));
        assert!(!is_nct6687dr_board("MAG Z790 GAMING PLUS WIFI (MS-7E06)"));
        assert!(is_nct6687dr_board("MAG X870 TOMAHAWK WIFI (MS-7E51)"));
        assert!(is_nct6687dr_board("mpg z890 carbon wifi"));
    }

    #[test]
    fn base_address_validation() {
        assert_eq!(validate_base(0xA20, 0xA20).ok(), Some(0xA20));
        assert!(validate_base(0xA20, 0xA28).is_err()); // unstable
        assert!(validate_base(0x0A0, 0x0A0).is_err()); // < 0x100
        assert!(validate_base(0xA25, 0xA25).is_err()); // not aligned
        assert!(validate_base(0xFFFF, 0xFFFF).is_err());
    }

    #[test]
    fn restore_touches_only_own_bit() {
        // Channel 2 was on auto (bit 0); channels 0 and 5 in manual otherwise.
        assert_eq!(restored_mode(0b0010_0101, 0b100, 0), 0b0010_0001);
        // Channel 2 was already manual originally: the bit stays 1.
        assert_eq!(restored_mode(0b0000_0000, 0b100, 0b100), 0b0000_0100);
    }
}
