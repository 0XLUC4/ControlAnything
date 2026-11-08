//! Intel i801/PCH SMBus host via the PawnIO `SmbusI801` module, behind a strict safety policy.
//!
//! What can reach the wire (enforced by `check_xfer`, the single gate of every transaction):
//! - "Receive Byte" (no write phase) on any 7-bit address: bus scan.
//! - "Read Byte Data" (command byte write, then read) ONLY on the Corsair DRAM ranges
//!   0x18..=0x1F and 0x58..=0x5F, and only on the registers the Corsair driver reads.
//! - "Write Byte Data" / "Write Block Data" ONLY on a Corsair address whose identification
//!   registers were read and validated by `identify_corsair`, and only on an allow-list of
//!   volatile registers (buffer select, direct color buffer). SPD hubs (0x50..=0x57), PMICs
//!   (0x48..=0x4F) and every other address can never be written.
//! - Never sent: Corsair 0x82 (write/apply configuration: hardware effects, which OpenRGB flags
//!   MODE_FLAG_AUTOMATIC_SAVE, i.e. persisted by the controller), 0x23 (switch to bootloader),
//!   0x26 / 0x20 / 0x0B (configuration buffer upload). They are absent from the allow-list.
//!
//! Sources (PawnIO.Modules tag 0.2.11, `SmbusI801.p`):
//! - `ioctl_smbus_xfer`: in = [address, read_write (READ=1, WRITE=0), command, protocol, data...],
//!   in_size >= 4. BYTE/BYTE_DATA write: in[4] = data byte; read: out[0] = byte read.
//!   BLOCK_DATA write: in_size >= 9, in[4..9] = bytes packed little-endian, byte 0 = length
//!   (1..=32), then the data (`unpack_bytes_le(in, in_data, 33, 4 * 8, 0)`).
//!   Its doc comment asks callers to hold the `Access_SMBUS.HTP.Method` mutant.
//! - In `i801_simple_transaction`, I2C_SMBUS_BYTE + READ only sets the slave address
//!   (SMBHSTADD with the read bit); SMBHSTCMD is written only for WRITE (Send Byte).
//! - `ioctl_identity`: out = ['i801' tag, I/O base, PCI ids | subsystem ids << 32].
//!
//! Same packing as OpenRGB `i2c_smbus/Windows/i2c_smbus_pawnio.cpp` (commit 029758e2): in[4..9] =
//! `i2c_smbus_data` union (byte, or block[0] = length + block[1..]). Mutex name
//! `Global\Access_SMBUS.HTP.Method`: `i2c_smbus_pawnio.h` (`GLOBAL_SMBUS_MUTEX_NAME`), held around
//! every `ioctl_smbus_xfer`.
//!
//! Corsair identification: OpenRGB `Controllers/CorsairDRAMController/CorsairDRAMControllerDetect.cpp`
//! (commit 029758e2): read byte data 0x43 in {0x1A, 0x1B, 0x1C} and 0x44 in {0x01, 0x03, 0x04},
//! addresses 0x58..=0x5F then 0x18..=0x1F.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::sync::atomic::{AtomicU16, Ordering};

use hub_core::HwError;
use windows::Win32::Foundation::{HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::w;

use crate::pawnio::PawnIo;

const I2C_SMBUS_READ: u64 = 1;
const I2C_SMBUS_WRITE: u64 = 0;
const I2C_SMBUS_BYTE: u64 = 1;
const I2C_SMBUS_BYTE_DATA: u64 = 2;
const I2C_SMBUS_BLOCK_DATA: u64 = 5;
pub const I2C_SMBUS_BLOCK_MAX: usize = 32;
const MUTEX_TIMEOUT_MS: u32 = 1000;

/// Corsair identification registers and accepted values (OpenRGB detector, see module doc).
const CORSAIR_REG_ID_A: u8 = 0x43;
const CORSAIR_REG_ID_B: u8 = 0x44;
/// Registers readable with Read Byte Data: identification, binary data (0x40), checksum (0x42).
const CORSAIR_READ_REGS: [u8; 4] = [0x40, 0x42, CORSAIR_REG_ID_A, CORSAIR_REG_ID_B];
/// Write Byte Data allow-list (register, value): select device info buffer (0x61) and start
/// binary transfer (0x21), both with 0x00. Volatile: they only select what 0x40 reads back.
const CORSAIR_BYTE_WRITES: [(u8, u8); 2] = [(0x61, 0x00), (0x21, 0x00)];
/// Write Block Data allow-list: direct color buffer blocks 1 and 2 (volatile, direct mode).
const CORSAIR_BLOCK_WRITES: [u8; 2] = [0x31, 0x32];

/// Intel PCH SMBus controller. Writes are limited to identified Corsair DRAM controllers.
pub struct SmbusI801 {
    pawn: PawnIo,
    smbus_mutex: OwnedHandle,
    /// Bit per Corsair address (see `corsair_bit`) validated by `identify_corsair`.
    identified: AtomicU16,
}

impl SmbusI801 {
    /// Opens PawnIO and loads the signed `SmbusI801.bin` module from `module_path`.
    /// The module's `main` only reads PCI config and maps the SMBus MMIO BAR.
    pub fn open(module_path: &Path) -> Result<Self, HwError> {
        // SAFETY: static null-terminated name, default security attributes.
        let raw = unsafe { CreateMutexW(None, false, w!(r"Global\Access_SMBUS.HTP.Method")) }
            .map_err(|err| HwError::Io(err.into()))?;
        // SAFETY: valid handle from CreateMutexW, exclusively owned here.
        let smbus_mutex = unsafe { OwnedHandle::from_raw_handle(raw.0) };
        Ok(Self { pawn: PawnIo::open(module_path)?, smbus_mutex, identified: AtomicU16::new(0) })
    }

    /// `ioctl_identity`: (I/O base, PCI vendor/device, subsystem vendor/device). PCI config reads only.
    pub fn identity(&self) -> Result<(u64, u32, u32), HwError> {
        let mut out = [0u64; 3];
        self.pawn.execute("ioctl_identity", &[], &mut out)?;
        Ok((out[1], out[2] as u32, (out[2] >> 32) as u32))
    }

    /// SMBus "Receive Byte" from the 7-bit `address`: START, addr+R, byte, NACK, STOP.
    pub fn receive_byte(&self, address: u8) -> Result<u8, HwError> {
        self.xfer(address, I2C_SMBUS_READ, 0, I2C_SMBUS_BYTE, &[])
    }

    /// SMBus "Read Byte Data" (Corsair ranges and registers only).
    pub fn read_byte_data(&self, address: u8, command: u8) -> Result<u8, HwError> {
        self.xfer(address, I2C_SMBUS_READ, command, I2C_SMBUS_BYTE_DATA, &[])
    }

    /// SMBus "Write Byte Data" (identified Corsair address, allow-listed register and value only).
    pub fn write_byte_data(&self, address: u8, command: u8, value: u8) -> Result<(), HwError> {
        self.xfer(address, I2C_SMBUS_WRITE, command, I2C_SMBUS_BYTE_DATA, &[value]).map(drop)
    }

    /// SMBus "Write Block Data", 1..=32 bytes (identified Corsair address, color buffer only).
    pub fn write_block_data(&self, address: u8, command: u8, data: &[u8]) -> Result<(), HwError> {
        self.xfer(address, I2C_SMBUS_WRITE, command, I2C_SMBUS_BLOCK_DATA, data).map(drop)
    }

    /// Reads Corsair identification registers 0x43/0x44 and returns them with the verdict.
    /// A valid answer unlocks the (allow-listed) writes on `address`.
    pub fn identify_corsair(&self, address: u8) -> Result<([u8; 2], bool), HwError> {
        let ids = [
            self.read_byte_data(address, CORSAIR_REG_ID_A)?,
            self.read_byte_data(address, CORSAIR_REG_ID_B)?,
        ];
        let valid = is_corsair_id(ids);
        if let (true, Some(bit)) = (valid, corsair_bit(address)) {
            self.identified.fetch_or(bit, Ordering::Relaxed);
        }
        Ok((ids, valid))
    }

    fn is_identified(&self, address: u8) -> bool {
        corsair_bit(address).is_some_and(|bit| self.identified.load(Ordering::Relaxed) & bit != 0)
    }

    /// The single bus transaction path: every request goes through `check_xfer` first.
    fn xfer(&self, address: u8, read_write: u64, command: u8, protocol: u64, data: &[u8]) -> Result<u8, HwError> {
        check_xfer(address, read_write, command, protocol, data, self.is_identified(address))?;
        let mut args = vec![u64::from(address), read_write, u64::from(command), protocol];
        match (read_write, protocol) {
            (I2C_SMBUS_WRITE, I2C_SMBUS_BYTE_DATA) => args.push(u64::from(data[0])),
            (I2C_SMBUS_WRITE, I2C_SMBUS_BLOCK_DATA) => args.extend(pack_block(data)),
            _ => {}
        }
        let _guard = self.lock_smbus()?;
        let mut out = [0u64; 1];
        self.pawn.execute("ioctl_smbus_xfer", &args, &mut out)?;
        Ok(out[0] as u8)
    }

    fn lock_smbus(&self) -> Result<SmbusGuard<'_>, HwError> {
        // SAFETY: valid mutex handle, owned by `self` which outlives the guard.
        let wait = unsafe { WaitForSingleObject(HANDLE(self.smbus_mutex.as_raw_handle()), MUTEX_TIMEOUT_MS) };
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            Ok(SmbusGuard(&self.smbus_mutex))
        } else {
            Err(HwError::Other("SMBus busy with another tool".into()))
        }
    }
}

/// Bit of a Corsair DRAM address in `identified`; `None` outside 0x18..=0x1F and 0x58..=0x5F.
fn corsair_bit(address: u8) -> Option<u16> {
    match address {
        0x18..=0x1F => Some(1 << (address - 0x18)),
        0x58..=0x5F => Some(1 << (address - 0x58 + 8)),
        _ => None,
    }
}

fn is_corsair_id([id_a, id_b]: [u8; 2]) -> bool {
    matches!(id_a, 0x1A..=0x1C) && matches!(id_b, 0x01 | 0x03 | 0x04)
}

/// Safety policy (pure, unit-tested). `identified` = address validated by `identify_corsair`.
fn check_xfer(
    address: u8,
    read_write: u64,
    command: u8,
    protocol: u64,
    data: &[u8],
    identified: bool,
) -> Result<(), HwError> {
    if address > 0x7F {
        return Err(HwError::Other(format!("invalid 7-bit address {address:#04x}")));
    }
    let corsair = corsair_bit(address).is_some();
    let allowed = match (read_write, protocol) {
        (I2C_SMBUS_READ, I2C_SMBUS_BYTE) => true,
        (I2C_SMBUS_READ, I2C_SMBUS_BYTE_DATA) => corsair && CORSAIR_READ_REGS.contains(&command),
        (I2C_SMBUS_WRITE, I2C_SMBUS_BYTE_DATA) => {
            corsair && identified && data.len() == 1 && CORSAIR_BYTE_WRITES.contains(&(command, data[0]))
        }
        (I2C_SMBUS_WRITE, I2C_SMBUS_BLOCK_DATA) => {
            corsair
                && identified
                && (1..=I2C_SMBUS_BLOCK_MAX).contains(&data.len())
                && CORSAIR_BLOCK_WRITES.contains(&command)
        }
        _ => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(HwError::Other(format!(
            "refused SMBus transaction: address {address:#04x}, command {command:#04x}, protocol {protocol}, read_write {read_write}"
        )))
    }
}

/// Block payload as `ioctl_smbus_xfer` expects it: 5 LE cells, byte 0 = length, then data.
fn pack_block(data: &[u8]) -> [u64; 5] {
    let mut bytes = [0u8; 40];
    bytes[0] = data.len() as u8;
    bytes[1..=data.len()].copy_from_slice(data);
    let mut cells = [0u64; 5];
    for (cell, chunk) in cells.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *cell = u64::from_le_bytes(*chunk);
    }
    cells
}

/// Releases the SMBus mutex on Drop (same thread as acquisition: local guard).
struct SmbusGuard<'a>(&'a OwnedHandle);

impl Drop for SmbusGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: mutex acquired by this thread in `lock_smbus`, handle still open.
        let _ = unsafe { ReleaseMutex(HANDLE(self.0.as_raw_handle())) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: u64 = I2C_SMBUS_READ;
    const W: u64 = I2C_SMBUS_WRITE;

    fn write_byte(address: u8, command: u8, value: u8, identified: bool) -> bool {
        check_xfer(address, W, command, I2C_SMBUS_BYTE_DATA, &[value], identified).is_ok()
    }

    fn write_block(address: u8, command: u8, len: usize, identified: bool) -> bool {
        check_xfer(address, W, command, I2C_SMBUS_BLOCK_DATA, &vec![0; len], identified).is_ok()
    }

    #[test]
    fn receive_byte_allowed_everywhere_but_send_byte_never() {
        assert!(check_xfer(0x50, R, 0, I2C_SMBUS_BYTE, &[], false).is_ok());
        assert!(check_xfer(0x50, W, 0, I2C_SMBUS_BYTE, &[0], true).is_err()); // Send Byte
        assert!(check_xfer(0x80, R, 0, I2C_SMBUS_BYTE, &[], false).is_err());
        for protocol in [0, 3, 4, 6, 7, 8] {
            assert!(check_xfer(0x19, R, 0x43, protocol, &[], true).is_err());
            assert!(check_xfer(0x19, W, 0x31, protocol, &[0], true).is_err());
        }
    }

    #[test]
    fn read_byte_data_only_in_corsair_ranges_and_registers() {
        for address in (0x18..=0x1F).chain(0x58..=0x5F) {
            assert!(check_xfer(address, R, 0x43, I2C_SMBUS_BYTE_DATA, &[], false).is_ok());
        }
        assert!(check_xfer(0x19, R, 0x00, I2C_SMBUS_BYTE_DATA, &[], true).is_err());
        for address in (0..0x18).chain(0x20..0x58).chain(0x60..=0x7F) {
            assert!(check_xfer(address, R, 0x43, I2C_SMBUS_BYTE_DATA, &[], true).is_err(), "{address:#x}");
        }
    }

    #[test]
    fn writes_refused_on_spd_pmic_and_everything_outside_corsair_ranges() {
        for address in (0..0x18).chain(0x20..0x58).chain(0x60..=0x7F) {
            assert!(!write_byte(address, 0x61, 0, true), "{address:#x}");
            assert!(!write_block(address, 0x31, 32, true), "{address:#x}");
        }
        for address in (0x50..=0x57).chain(0x48..=0x4F) {
            assert!(!write_byte(address, 0x21, 0, true));
            assert!(!write_block(address, 0x32, 6, true));
        }
    }

    #[test]
    fn writes_need_identification_and_allow_listed_registers() {
        assert!(write_byte(0x19, 0x61, 0, true));
        assert!(write_byte(0x5B, 0x21, 0, true));
        assert!(write_block(0x19, 0x31, 32, true));
        assert!(write_block(0x19, 0x32, 6, true));
        assert!(!write_byte(0x19, 0x61, 0, false));
        assert!(!write_block(0x19, 0x31, 32, false));
        // Persistent / dangerous Corsair registers are never reachable.
        for (command, value) in [(0x82, 1), (0x82, 2), (0x23, 0), (0x26, 0), (0x20, 0), (0x0B, 0), (0x61, 1)] {
            assert!(!write_byte(0x19, command, value, true), "{command:#x}");
        }
        assert!(!write_block(0x19, 0x82, 4, true));
        assert!(!write_block(0x19, 0x31, 0, true));
        assert!(!write_block(0x19, 0x31, 33, true));
    }

    #[test]
    fn identification_values_and_addresses() {
        assert!(is_corsair_id([0x1A, 0x04]) && is_corsair_id([0x1C, 0x01]) && is_corsair_id([0x1B, 0x03]));
        assert!(!is_corsair_id([0x19, 0x04]) && !is_corsair_id([0x1B, 0x02]) && !is_corsair_id([0xFF, 0xFF]));
        assert_eq!(corsair_bit(0x18), Some(1));
        assert_eq!(corsair_bit(0x5F), Some(1 << 15));
        assert_eq!(corsair_bit(0x50), None);
        assert_eq!(corsair_bit(0x4F), None);
    }

    #[test]
    fn block_is_packed_length_first_little_endian() {
        let cells = pack_block(&[1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(cells[0], u64::from_le_bytes([9, 1, 2, 3, 4, 5, 6, 7]));
        assert_eq!(cells[1], u64::from_le_bytes([8, 9, 0, 0, 0, 0, 0, 0]));
        assert_eq!(cells[2..], [0, 0, 0]);
        let full = pack_block(&[0xAB; 32]);
        assert_eq!(full[3], u64::from_le_bytes([0xAB; 8]));
        assert_eq!(full[4], 0xAB); // byte 32 = last data byte
        assert_eq!(full[0] & 0xFF, 32);
    }
}
