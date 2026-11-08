//! Intel i801/PCH SMBus host via the PawnIO `SmbusI801` module. READ-ONLY by design:
//! the only bus transaction exposed is SMBus "Receive Byte" (no write phase on the wire).
//!
//! Sources (PawnIO.Modules tag 0.2.11, `SmbusI801.p`):
//! - `ioctl_smbus_xfer`: in = [address, read_write (READ=1, WRITE=0), command, protocol, data...],
//!   in_size >= 4; out[0] = byte read for I2C_SMBUS_BYTE (protocol 1) with READ.
//!   Its doc comment asks callers to hold the `Access_SMBUS.HTP.Method` mutant.
//! - In `i801_simple_transaction`, I2C_SMBUS_BYTE + READ only sets the slave address
//!   (SMBHSTADD with the read bit); SMBHSTCMD is written only for WRITE (Send Byte).
//! - `ioctl_identity`: out = ['i801' tag, I/O base, PCI ids | subsystem ids << 32].
//!
//! Mutex name `Global\Access_SMBUS.HTP.Method`: OpenRGB `i2c_smbus/Windows/i2c_smbus_pawnio.h`
//! (`GLOBAL_SMBUS_MUTEX_NAME`), held around every `ioctl_smbus_xfer` in `i2c_smbus_pawnio.cpp`.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use hub_core::HwError;
use windows::Win32::Foundation::{HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::w;

use crate::pawnio::PawnIo;

const I2C_SMBUS_READ: u64 = 1;
const I2C_SMBUS_BYTE: u64 = 1;
const MUTEX_TIMEOUT_MS: u32 = 1000;

/// Read-only handle on the Intel PCH SMBus controller.
pub struct SmbusI801 {
    pawn: PawnIo,
    smbus_mutex: OwnedHandle,
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
        Ok(Self { pawn: PawnIo::open(module_path)?, smbus_mutex })
    }

    /// `ioctl_identity`: (I/O base, PCI vendor/device, subsystem vendor/device). PCI config reads only.
    pub fn identity(&self) -> Result<(u64, u32, u32), HwError> {
        let mut out = [0u64; 3];
        self.pawn.execute("ioctl_identity", &[], &mut out)?;
        Ok((out[1], out[2] as u32, (out[2] >> 32) as u32))
    }

    /// SMBus "Receive Byte" from the 7-bit `address`: START, addr+R, byte, NACK, STOP.
    pub fn receive_byte(&self, address: u8) -> Result<u8, HwError> {
        self.xfer(address, I2C_SMBUS_READ, I2C_SMBUS_BYTE)
    }

    /// The single bus transaction path. Safety guard: refuses everything except
    /// read_write=READ with protocol=BYTE, so no write phase can ever reach the bus.
    fn xfer(&self, address: u8, read_write: u64, protocol: u64) -> Result<u8, HwError> {
        check_receive_byte(address, read_write, protocol)?;
        let _guard = self.lock_smbus()?;
        let mut out = [0u64; 1];
        // Command (in[2]) is ignored by the module for BYTE+READ; 0 for clarity.
        self.pawn.execute("ioctl_smbus_xfer", &[u64::from(address), read_write, 0, protocol], &mut out)?;
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

fn check_receive_byte(address: u8, read_write: u64, protocol: u64) -> Result<(), HwError> {
    if read_write != I2C_SMBUS_READ || protocol != I2C_SMBUS_BYTE {
        return Err(HwError::Other("refused: only SMBus Receive Byte is allowed".into()));
    }
    if address > 0x7F {
        return Err(HwError::Other(format!("invalid 7-bit address {address:#04x}")));
    }
    Ok(())
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

    #[test]
    fn guard_allows_only_receive_byte() {
        assert!(check_receive_byte(0x50, I2C_SMBUS_READ, I2C_SMBUS_BYTE).is_ok());
        assert!(check_receive_byte(0x50, 0, I2C_SMBUS_BYTE).is_err()); // WRITE (Send Byte)
        for protocol in [0, 2, 3, 4, 5, 6, 7, 8] {
            assert!(check_receive_byte(0x50, I2C_SMBUS_READ, protocol).is_err());
        }
        assert!(check_receive_byte(0x80, I2C_SMBUS_READ, I2C_SMBUS_BYTE).is_err());
    }
}
