//! PawnIO driver access: one handle = one loaded signed module.

use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use hub_core::HwError;
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::core::w;

const IOCTL_LOAD: u32 = (41394 << 16) | (0x821 << 2);
const IOCTL_EXEC: u32 = (41394 << 16) | (0x841 << 2);
const FN_NAME_LEN: usize = 32;

/// PawnIO handle with a loaded module. Closed on Drop.
pub(crate) struct PawnIo {
    handle: OwnedHandle,
}

impl PawnIo {
    /// Opens `\\?\GLOBALROOT\Device\PawnIO` and loads the signed module read from `module_path`.
    pub(crate) fn open(module_path: &Path) -> Result<Self, HwError> {
        let module = std::fs::read(module_path)?;
        // SAFETY: static null-terminated UTF-16 string, no pointer retained.
        let raw = unsafe {
            CreateFileW(
                w!(r"\\?\GLOBALROOT\Device\PawnIO"),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        // Raw Win32 code (HRESULT 0x8007xxxx): otherwise `kind()` is Uncategorized.
        .map_err(|err| match std::io::Error::from_raw_os_error(err.code().0 & 0xFFFF) {
            e if e.kind() == std::io::ErrorKind::PermissionDenied => {
                HwError::PermissionDenied("PawnIO".into())
            }
            e if e.kind() == std::io::ErrorKind::NotFound => {
                HwError::Unavailable("PawnIO not installed".into())
            }
            e => HwError::Io(e),
        })?;
        // SAFETY: valid handle from CreateFileW, exclusively owned here.
        let pawn = Self { handle: unsafe { OwnedHandle::from_raw_handle(raw.0) } };
        pawn.ioctl(IOCTL_LOAD, &module, &mut [])?;
        Ok(pawn)
    }

    /// Calls the module function `name`: input = name (32 bytes) + `args`,
    /// output = exactly `out.len()` 64-bit cells.
    pub(crate) fn execute(&self, name: &str, args: &[u64], out: &mut [u64]) -> Result<(), HwError> {
        let input = exec_input(name, args)?;
        let mut raw_out = vec![0u8; out.len() * 8];
        let read = self.ioctl(IOCTL_EXEC, &input, &mut raw_out)?;
        if read != raw_out.len() {
            return Err(HwError::Other(format!("PawnIO {name}: {read}-byte response")));
        }
        for (cell, bytes) in out.iter_mut().zip(raw_out.as_chunks::<8>().0) {
            *cell = u64::from_le_bytes(*bytes);
        }
        Ok(())
    }

    fn ioctl(&self, code: u32, input: &[u8], output: &mut [u8]) -> Result<usize, HwError> {
        let mut returned = 0u32;
        // SAFETY: buffers valid during the synchronous call, exact sizes passed to the driver.
        unsafe {
            DeviceIoControl(
                HANDLE(self.handle.as_raw_handle()),
                code,
                Some(input.as_ptr().cast::<c_void>()),
                input.len() as u32,
                (!output.is_empty()).then(|| output.as_mut_ptr().cast::<c_void>()),
                output.len() as u32,
                Some(&mut returned),
                None,
            )
        }
        .map_err(|err| HwError::Io(err.into()))?;
        Ok(returned as usize)
    }
}

/// IOCTL_EXEC input buffer: null-terminated ASCII name on 32 bytes, then LE arguments.
fn exec_input(name: &str, args: &[u64]) -> Result<Vec<u8>, HwError> {
    if name.len() >= FN_NAME_LEN {
        return Err(HwError::Other(format!("PawnIO function name too long: {name}")));
    }
    let mut input = vec![0u8; FN_NAME_LEN + args.len() * 8];
    input[..name.len()].copy_from_slice(name.as_bytes());
    for (slot, arg) in input[FN_NAME_LEN..].as_chunks_mut::<8>().0.iter_mut().zip(args) {
        slot.copy_from_slice(&arg.to_le_bytes());
    }
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_input_layout() {
        let input = exec_input("ioctl_pio_outb", &[0x2E, 0x87]).expect("input");
        assert_eq!(input.len(), 48);
        assert_eq!(&input[..14], b"ioctl_pio_outb");
        assert!(input[14..32].iter().all(|&b| b == 0));
        assert_eq!(input[32..40], 0x2Eu64.to_le_bytes());
        assert_eq!(input[40..48], 0x87u64.to_le_bytes());
        assert!(exec_input(&"x".repeat(32), &[]).is_err());
    }
}
