//! Corsair DRAM RGB controller (Vengeance RGB DDR5 and siblings) on the Intel PCH SMBus,
//! direct mode only (volatile colors).
//!
//! Protocol facts, OpenRGB `Controllers/CorsairDRAMController/` at commit 029758e2 (GPL-2.0-or-later,
//! read for the facts, code rewritten):
//! - Detection (`CorsairDRAMControllerDetect.cpp`): see `SmbusI801::identify_corsair`.
//! - Device info (`CorsairDRAMController.cpp`, `ReadDeviceInfo`): write byte 0x61 = 0x00 (select
//!   device info buffer), write byte 0x21 = 0x00 (start binary transfer), 32 x read byte 0x40,
//!   then read byte 0x42 = CRC8 of the 32 bytes. VID = [1]:[0], PID = [3]:[2], protocol
//!   version = [28]. Direct mode needs protocol >= 4.
//! - Models (`CorsairDRAMDevices.cpp`, `.h`): PID -> name, LED count, reversed LED order; VID 0x1B1C.
//! - Direct frame (`SetColorsPerLED`): [LED count, R, G, B per LED..., CRC8 of the previous bytes],
//!   first 32 bytes as SMBus block write to 0x31, the rest (if any) as block write to 0x32.
//! - CRC8 = CRCpp `CRC_8()` (`dependencies/CRCpp/CRC.h`): poly 0x07, init 0x00, no reflection,
//!   xorout 0x00, check("123456789") = 0xF4.
//!
//! Safety:
//! - Hardware effects (`SetEffect`: config upload + write 0x82) are flagged
//!   MODE_FLAG_AUTOMATIC_SAVE by OpenRGB (`RGBController_CorsairDRAM.cpp`), i.e. persisted by the
//!   controller: FORBIDDEN here, and refused by the `SmbusI801` allow-list anyway.
//! - Opening writes 0x61 and 0x21 (buffer selection for the device info read, as OpenRGB's
//!   constructor does). Neither changes the LEDs; nothing else is written before `set_frame`.
//! - `restore` cannot hand the LEDs back to the firmware: OpenRGB has no volatile way out of
//!   direct mode (leaving it = `SetEffect`, persisted). The last colors stay until another tool
//!   (iCUE, OpenRGB) sets an effect; behavior after a power cycle is not verified.
//!
//! Frame cost (theory): one 32-byte block write = START + addr + command + count + 32 bytes
//! = 35 bytes x 9 bits = 315 bit clocks, ~3.2 ms at 100 kHz (+ ~6 bytes for 12-LED models),
//! plus the PawnIO call. At 15 frames/s and 2 DIMMs: ~10 % of the bus.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hub_core::HwError;
use hub_core::rgb::{Rgb, RgbDevice, RgbDeviceInfo, RgbZone};

use crate::smbus_i801::{I2C_SMBUS_BLOCK_MAX, SmbusI801};

const VID_CORSAIR: u16 = 0x1B1C;
const REG_SELECT_DEVICE_INFO: u8 = 0x61;
const REG_BINARY_START: u8 = 0x21;
const REG_BINARY_DATA: u8 = 0x40;
const REG_CHECKSUM: u8 = 0x42;
const REG_COLOR_BLOCK_1: u8 = 0x31;
const REG_COLOR_BLOCK_2: u8 = 0x32;
const MIN_PROTOCOL_DIRECT: u8 = 4;
/// Detection order of OpenRGB: 0x58..=0x5F, then 0x18..=0x1F.
const CANDIDATES: [std::ops::RangeInclusive<u8>; 2] = [0x58..=0x5F, 0x18..=0x1F];
/// <= ~15 frames/s: the SMBus is slow and shared with SPD/PMIC traffic.
pub const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(66);

/// Known model (OpenRGB `CorsairDRAMDevices.cpp`).
struct Model {
    name: &'static str,
    pids: &'static [u16],
    leds: usize,
    reverse: bool,
}

/// shortcut: 6-LED models (Vengeance RGB RS DDR4/DDR5) are left out because OpenRGB always sends a
/// 32-byte first block, longer than their 20-byte frame; add them after checking on hardware.
const MODELS: [Model; 7] = [
    Model { name: "Corsair Vengeance RGB Pro DDR4", pids: &[0x0100, 0x0101], leds: 10, reverse: false },
    Model { name: "Corsair Dominator Platinum RGB DDR4", pids: &[0x0200, 0x0201], leds: 12, reverse: true },
    Model { name: "Corsair Vengeance RGB Pro SL DDR4", pids: &[0x0300, 0x0301], leds: 10, reverse: false },
    Model { name: "Corsair Dominator Platinum RGB DDR5", pids: &[0x0600, 0x0601], leds: 12, reverse: true },
    Model {
        name: "Corsair Dominator Titanium RGB DDR5",
        pids: &[0x0800, 0x0801, 0x0810, 0x0811],
        leds: 12,
        reverse: true,
    },
    Model {
        name: "Corsair Vengeance RGB DDR5",
        pids: &[0x0700, 0x0701, 0x0900, 0x0901, 0x0910, 0x0911],
        leds: 10,
        reverse: false,
    },
    Model {
        name: "Corsair Vengeance Shugo Series DDR5",
        pids: &[0x0A00, 0x0A01, 0x0A10, 0x0A11],
        leds: 10,
        reverse: false,
    },
];

/// Parsed device info buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DeviceInfo {
    vid: u16,
    pid: u16,
    protocol: u8,
}

/// CRC-8/SMBus: poly 0x07, init 0x00, no reflection, no final XOR.
fn crc8(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |crc, byte| {
        (0..8).fold(crc ^ byte, |crc, _| if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 })
    })
}

fn parse_device_info(raw: &[u8; 32]) -> DeviceInfo {
    DeviceInfo {
        vid: u16::from_le_bytes([raw[0], raw[1]]),
        pid: u16::from_le_bytes([raw[2], raw[3]]),
        protocol: raw[28],
    }
}

fn find_model(pid: u16) -> Option<&'static Model> {
    MODELS.iter().find(|model| model.pids.contains(&pid))
}

/// Direct-mode frame: LED count, R/G/B per LED (reversed on some models), CRC8.
fn build_packet(model: &Model, colors: &[Rgb]) -> Result<Vec<u8>, HwError> {
    if colors.len() != model.leds {
        return Err(HwError::Other(format!("{} LEDs given, {} expected", colors.len(), model.leds)));
    }
    let mut packet = Vec::with_capacity(model.leds * 3 + 2);
    packet.push(model.leds as u8);
    for index in 0..model.leds {
        let color = colors[if model.reverse { model.leds - 1 - index } else { index }];
        packet.extend([color.r, color.g, color.b]);
    }
    packet.push(crc8(&packet));
    Ok(packet)
}

/// Reads the device info buffer (writes 0x61/0x21 = buffer selection only) and checks its CRC.
fn read_device_info(bus: &SmbusI801, address: u8) -> Result<DeviceInfo, HwError> {
    bus.write_byte_data(address, REG_SELECT_DEVICE_INFO, 0x00)?;
    bus.write_byte_data(address, REG_BINARY_START, 0x00)?;
    let mut raw = [0u8; 32];
    for byte in &mut raw {
        *byte = bus.read_byte_data(address, REG_BINARY_DATA)?;
    }
    let device_crc = bus.read_byte_data(address, REG_CHECKSUM)?;
    if crc8(&raw) != device_crc {
        return Err(HwError::Other(format!("device info CRC mismatch at {address:#04x}: {raw:02x?}")));
    }
    Ok(parse_device_info(&raw))
}

/// One Corsair DIMM in direct mode.
pub struct CorsairDram {
    info: RgbDeviceInfo,
    bus: Arc<SmbusI801>,
    address: u8,
    model: &'static Model,
    /// Last write attempt and the frame it sent (empty if it failed).
    last_sent: Option<(Instant, Vec<u8>)>,
}

impl CorsairDram {
    /// Identifies the controller, reads its device info and checks the model. No LED change.
    pub fn open(bus: Arc<SmbusI801>, address: u8) -> Result<Self, HwError> {
        let (ids, valid) = bus.identify_corsair(address)?;
        if !valid {
            return Err(HwError::Unavailable(format!("{address:#04x}: not a Corsair DRAM controller ({ids:02x?})")));
        }
        let device = read_device_info(&bus, address)?;
        let model = find_model(device.pid)
            .filter(|_| device.vid == VID_CORSAIR)
            .ok_or_else(|| HwError::Unavailable(format!("{address:#04x}: unsupported device {device:?}")))?;
        if device.protocol < MIN_PROTOCOL_DIRECT {
            return Err(HwError::Unavailable(format!(
                "{address:#04x}: protocol {} has no direct mode",
                device.protocol
            )));
        }
        let info = RgbDeviceInfo {
            id: format!("corsair-dram/{address:#04x}"),
            label: format!("{} (DIMM {address:#04x})", model.name),
            zones: vec![RgbZone { id: "leds".into(), label: "LEDs".into(), leds: model.leds }],
        };
        Ok(Self { info, bus, address, model, last_sent: None })
    }
}

impl RgbDevice for CorsairDram {
    fn info(&self) -> &RgbDeviceInfo {
        &self.info
    }

    fn set_frame(&mut self, frame: &[Vec<Rgb>]) -> Result<(), HwError> {
        let [leds] = frame else {
            return Err(HwError::Other(format!("frame has {} zones, 1 expected", frame.len())));
        };
        let packet = build_packet(self.model, leds)?;
        let now = Instant::now();
        if let Some((at, last)) = &self.last_sent
            && (*last == packet || now.duration_since(*at) < MIN_FRAME_INTERVAL)
        {
            return Ok(());
        }
        self.last_sent = Some((now, Vec::new()));
        let (first, rest) = packet.split_at(packet.len().min(I2C_SMBUS_BLOCK_MAX));
        self.bus.write_block_data(self.address, REG_COLOR_BLOCK_1, first)?;
        if !rest.is_empty() {
            self.bus.write_block_data(self.address, REG_COLOR_BLOCK_2, rest)?;
        }
        self.last_sent = Some((now, packet));
        Ok(())
    }

    /// No volatile way back to the firmware effect exists (see module doc): writes nothing,
    /// the last colors stay. The next `set_frame` is sent immediately.
    fn restore(&mut self) {
        self.last_sent = None;
    }
}

impl Drop for CorsairDram {
    fn drop(&mut self) {
        self.restore();
    }
}

/// SMBus address and the result of opening the DIMM there.
pub type DetectedDimm = (u8, Result<CorsairDram, HwError>);

/// Opens the SMBus and every identified Corsair DIMM. Addresses that do not answer or do not
/// identify as Corsair are skipped; identified ones that fail to open are returned as errors.
pub fn detect_corsair_dram(module_path: &Path) -> Result<Vec<DetectedDimm>, HwError> {
    let bus = Arc::new(SmbusI801::open(module_path)?);
    let mut found = Vec::new();
    for address in CANDIDATES.into_iter().flatten() {
        if let Ok((_, true)) = bus.identify_corsair(address) {
            found.push((address, CorsairDram::open(Arc::clone(&bus), address)));
        }
        std::thread::sleep(Duration::from_millis(10)); // same pacing as the OpenRGB detector
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc8_matches_crcpp_check_value() {
        assert_eq!(crc8(b"123456789"), 0xF4);
        assert_eq!(crc8(&[]), 0x00);
        assert_eq!(crc8(&[0x00]), 0x00);
        assert_eq!(crc8(&[0x01]), 0x07);
    }

    #[test]
    fn packet_layout_for_vengeance_ddr5() {
        let model = find_model(0x0911).expect("known PID");
        assert_eq!((model.name, model.leds, model.reverse), ("Corsair Vengeance RGB DDR5", 10, false));
        let mut colors = vec![Rgb::BLACK; 10];
        colors[0] = Rgb::new(1, 2, 3);
        colors[9] = Rgb::new(7, 8, 9);
        let packet = build_packet(model, &colors).expect("packet");
        assert_eq!(packet.len(), 32); // one block write
        assert_eq!(packet[..4], [10, 1, 2, 3]);
        assert_eq!(packet[28..31], [7, 8, 9]);
        assert_eq!(packet[31], crc8(&packet[..31]));
        assert!(build_packet(model, &colors[..9]).is_err());
    }

    #[test]
    fn packet_reversed_and_split_for_dominator() {
        let model = find_model(0x0600).expect("known PID");
        let mut colors = vec![Rgb::BLACK; 12];
        colors[11] = Rgb::new(4, 5, 6);
        let packet = build_packet(model, &colors).expect("packet");
        assert_eq!(packet.len(), 38); // 32 to 0x31 + 6 to 0x32
        assert_eq!(packet[..4], [12, 4, 5, 6]);
    }

    #[test]
    fn device_info_fields() {
        let mut raw = [0u8; 32];
        raw[..4].copy_from_slice(&[0x1C, 0x1B, 0x11, 0x09]);
        raw[28] = 4;
        let info = parse_device_info(&raw);
        assert_eq!(info, DeviceInfo { vid: VID_CORSAIR, pid: 0x0911, protocol: 4 });
        assert!(find_model(0x0B00).is_none()); // Vengeance RGB RS DDR5: not supported
    }
}
