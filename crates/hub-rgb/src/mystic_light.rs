//! MSI Mystic Light driver (USB HID), "185-byte" family.
//!
//! Protocol facts (sources: description of the `FeaturePacket_185` and `ZoneData`
//! structures, board table and detection in OpenRGB
//! `Controllers/MSIMotherboardController/*`, GPL-2.0, read for the facts, code rewritten):
//! - Feature report id 0x52, 185 bytes (id included). Detection reads this report with a
//!   200-byte buffer; a length of 185 (or 186) identifies the 185 family.
//! - Zone = 10 bytes: effect, R, G, B, speed|brightness, R2, G2, B2, color flags,
//!   padding; JRAINBOW zones have an 11th byte (LED count / cycle).
//! - Offsets: JRGB1 = 1, JRAINBOW1 = 31, JRAINBOW2 = 42, ON_BOARD_LED = 74, byte 184 =
//!   "save to flash" flag (1 = writes flash). We ALWAYS send it as 0.
//! - PID 0x7E06 (MS-7E06, Z790): 185 family, zones JRGB1 + JRAINBOW1 + JRAINBOW2,
//!   no onboard LED; without onboard LEDs, the ON_BOARD_LED zone must mirror JRGB.
//! - Effect 1 = static; brightness 100 % = 10 << 2; bit 7 of the color flags =
//!   custom color.
//!
//! Safety choice: only report 0x52 (volatile, save byte at 0) is sent, in
//! "one color per zone" mode. Per-LED mode (report 0x53) is not used.
//! shortcut: one color per ARGB header, move to per-LED report 0x53 after hardware validation.

use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};
use hub_core::HwError;
use hub_core::rgb::{Rgb, RgbDevice, RgbDeviceInfo, RgbZone};

pub const VID: u16 = 0x1462;
/// Strict allow-list: only boards whose 185 family is verified.
pub const ALLOWED_PIDS: &[u16] = &[0x7E06];
pub const REPORT_ID: u8 = 0x52;
pub const REPORT_LEN: usize = 185;
/// Read buffer larger than the report (same method as the reference detection).
pub const READ_BUF_LEN: usize = 200;
const SAVE_FLAG_OFFSET: usize = 184;
const ON_BOARD_OFFSET: usize = 74;
const MODE_STATIC: u8 = 1;
const BRIGHTNESS_100: u8 = 10 << 2;
const COLOR_FLAG_CUSTOM: u8 = 0x80;
const COLOR_FLAG_SYNC_ONBOARD: u8 = 0x01;
/// JPIPE1/JPIPE2 sync bits of the onboard zone (useless in static mode).
const COLOR_FLAG_SYNC_JPIPE: u8 = 0x30;
const SPEED_FLAG_SYNC_JRGB: u8 = 0x80;
/// <= 30 frames/s.
pub const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(33);

pub type Report = [u8; REPORT_LEN];

/// Exposed zone and its offset in the report.
pub struct ZoneSlot {
    pub id: &'static str,
    pub label: &'static str,
    pub offset: usize,
}

/// MS-7E06 zones, in `info().zones` order.
pub const ZONES: [ZoneSlot; 3] = [
    ZoneSlot { id: "jrgb1", label: "JRGB", offset: 1 },
    ZoneSlot { id: "jargb1", label: "JARGB_V2 1", offset: 31 },
    ZoneSlot { id: "jargb2", label: "JARGB_V2 2", offset: 42 },
];

pub fn is_allowed(vid: u16, pid: u16) -> bool {
    vid == VID && ALLOWED_PIDS.contains(&pid)
}

/// Validates a read report (`read` = bytes returned by `get_feature_report`).
/// Accepts 185 or 186 bytes (some drivers add a byte), id 0x52.
pub fn parse_report(read: &[u8]) -> Result<Report, String> {
    if read.len() != REPORT_LEN && read.len() != REPORT_LEN + 1 {
        return Err(format!("unexpected length {} (expected {REPORT_LEN})", read.len()));
    }
    if read[0] != REPORT_ID {
        return Err(format!("unexpected report id 0x{:02x}", read[0]));
    }
    let mut report = [0u8; REPORT_LEN];
    report.copy_from_slice(&read[..REPORT_LEN]);
    // Never save to flash, even when restoring.
    report[SAVE_FLAG_OFFSET] = 0;
    Ok(report)
}

/// Color (effect, R, G, B) of a zone as read from the report.
pub fn zone_state(report: &Report, offset: usize) -> (u8, Rgb) {
    (report[offset], Rgb::new(report[offset + 1], report[offset + 2], report[offset + 3]))
}

fn write_static(report: &mut Report, offset: usize, color: Rgb) {
    report[offset] = MODE_STATIC;
    report[offset + 1..offset + 4].copy_from_slice(&[color.r, color.g, color.b]);
    report[offset + 4] = BRIGHTNESS_100;
    report[offset + 5..offset + 8].copy_from_slice(&[color.r, color.g, color.b]);
    report[offset + 8] |= COLOR_FLAG_CUSTOM;
}

/// Builds the "static per zone" report from the original state.
/// Unrelated bytes (ARGB LED count, absent zones) keep their original values.
pub fn build_report(original: &Report, colors: &[Rgb; ZONES.len()]) -> Report {
    let mut report = *original;
    for (slot, color) in ZONES.iter().zip(colors) {
        write_static(&mut report, slot.offset, *color);
    }
    // Board without onboard LED: the onboard zone drives JRGB, so mirror it.
    let onboard = ON_BOARD_OFFSET;
    report[onboard] = MODE_STATIC;
    report.copy_within(2..5, onboard + 1);
    report.copy_within(6..9, onboard + 5);
    report[onboard + 4] &= !SPEED_FLAG_SYNC_JRGB;
    report[onboard + 8] = (report[onboard + 8] | COLOR_FLAG_CUSTOM | COLOR_FLAG_SYNC_ONBOARD)
        & !COLOR_FLAG_SYNC_JPIPE;
    report[onboard + 9] = 0;
    report[SAVE_FLAG_OFFSET] = 0;
    report
}

/// Limiter: at most one write per `MIN_FRAME_INTERVAL`, nothing if identical to the last send.
#[derive(Default)]
pub struct FrameGate {
    last_sent: Option<(Instant, Report)>,
}

impl FrameGate {
    pub fn should_send(&self, report: &Report, now: Instant) -> bool {
        match &self.last_sent {
            None => true,
            Some((at, last)) => last != report && now.duration_since(*at) >= MIN_FRAME_INTERVAL,
        }
    }

    pub fn mark_sent(&mut self, report: Report, now: Instant) {
        self.last_sent = Some((now, report));
    }

    pub fn reset(&mut self) {
        self.last_sent = None;
    }
}

/// Converts `frame[zone][led]` to one color per zone (LED 0).
pub fn frame_colors(frame: &[Vec<Rgb>]) -> Result<[Rgb; ZONES.len()], HwError> {
    if frame.len() != ZONES.len() {
        return Err(HwError::Other(format!(
            "frame has {} zones, {} expected",
            frame.len(),
            ZONES.len()
        )));
    }
    let mut colors = [Rgb::BLACK; ZONES.len()];
    for (index, leds) in frame.iter().enumerate() {
        // One LED per zone: extra LEDs are ignored.
        colors[index] = *leds
            .first()
            .ok_or_else(|| HwError::Other(format!("zone {} empty", ZONES[index].id)))?;
    }
    Ok(colors)
}

/// Reads the current report (read-only).
pub fn read_report(device: &HidDevice) -> Result<Vec<u8>, String> {
    let mut buf = [0u8; READ_BUF_LEN];
    buf[0] = REPORT_ID;
    let len = device.get_feature_report(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf[..len.min(READ_BUF_LEN)].to_vec())
}

pub struct MysticLight {
    info: RgbDeviceInfo,
    device: HidDevice,
    original: Report,
    gate: FrameGate,
    /// True once a write has happened: `restore` only writes in that case.
    dirty: bool,
}

impl MysticLight {
    fn open(api: &HidApi, path: &std::ffi::CStr, pid: u16) -> Result<Self, String> {
        let device = api.open_path(path).map_err(|e| e.to_string())?;
        let original = parse_report(&read_report(&device)?)?;
        let zones = ZONES
            .iter()
            .map(|slot| RgbZone { id: slot.id.into(), label: slot.label.into(), leds: 1 })
            .collect();
        let info = RgbDeviceInfo {
            id: format!("mystic-light/{VID:04x}:{pid:04x}"),
            label: format!("MSI Mystic Light (MS-{pid:04X})"),
            zones,
        };
        Ok(Self { info, device, original, gate: FrameGate::default(), dirty: false })
    }

    fn send(&self, report: &Report) -> Result<(), HwError> {
        self.device
            .send_feature_report(report)
            .map_err(|e| HwError::Unavailable(format!("Mystic Light: {e}")))
    }
}

impl RgbDevice for MysticLight {
    fn info(&self) -> &RgbDeviceInfo {
        &self.info
    }

    fn set_frame(&mut self, frame: &[Vec<Rgb>]) -> Result<(), HwError> {
        let report = build_report(&self.original, &frame_colors(frame)?);
        let now = Instant::now();
        if !self.gate.should_send(&report, now) {
            return Ok(());
        }
        self.dirty = true;
        self.send(&report)?;
        self.gate.mark_sent(report, now);
        Ok(())
    }

    fn restore(&mut self) {
        if !self.dirty {
            return;
        }
        match self.send(&self.original) {
            Ok(()) => self.dirty = false,
            Err(e) => tracing::warn!("Mystic Light restore failed: {e}"),
        }
        self.gate.reset();
    }
}

impl Drop for MysticLight {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Opens the allowed controllers; no write.
pub fn detect() -> Vec<Box<dyn RgbDevice>> {
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            tracing::warn!("hidapi unavailable: {e}");
            return Vec::new();
        }
    };
    let mut found: Vec<Box<dyn RgbDevice>> = Vec::new();
    for info in api.device_list() {
        if !is_allowed(info.vendor_id(), info.product_id()) {
            continue;
        }
        // Open each PID once (Windows may expose several collections).
        let id = format!("mystic-light/{VID:04x}:{:04x}", info.product_id());
        if found.iter().any(|device| device.info().id == id) {
            continue;
        }
        match MysticLight::open(&api, info.path(), info.product_id()) {
            Ok(device) => found.push(Box::new(device)),
            Err(e) => tracing::warn!(
                "Mystic Light {:04x} skipped ({:?}): {e}",
                info.product_id(),
                info.path()
            ),
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic report consistent with the documented layout.
    fn fixture() -> Vec<u8> {
        let mut report = vec![0u8; REPORT_LEN];
        report[0] = REPORT_ID;
        // JRAINBOW1: rainbow (9), 0x28 brightness, 40 LEDs.
        report[31] = 9;
        report[35] = 0x28;
        report[41] = 40;
        report[52] = 60; // JRAINBOW2: 60 LEDs
        report[ON_BOARD_OFFSET + 4] = 0xA9;
        report[ON_BOARD_OFFSET + 8] = 0xB1;
        report[SAVE_FLAG_OFFSET] = 1;
        report
    }

    #[test]
    fn allow_list_only_7e06() {
        assert!(is_allowed(0x1462, 0x7E06));
        assert!(!is_allowed(0x1462, 0x7E07));
        assert!(!is_allowed(0x1463, 0x7E06));
    }

    #[test]
    fn parse_validates_and_clears_save_flag() {
        let report = parse_report(&fixture()).unwrap();
        assert_eq!(report[SAVE_FLAG_OFFSET], 0);
        assert_eq!(zone_state(&report, 31).0, 9);
        let mut longer = fixture();
        longer.push(0);
        assert!(parse_report(&longer).is_ok());
        assert!(parse_report(&fixture()[..162]).is_err());
        let mut wrong_id = fixture();
        wrong_id[0] = 0x50;
        assert!(parse_report(&wrong_id).is_err());
    }

    #[test]
    fn build_layout() {
        let original = parse_report(&fixture()).unwrap();
        let red = Rgb::new(255, 0, 0);
        let blue = Rgb::new(0, 0, 255);
        let report = build_report(&original, &[red, Rgb::new(0, 255, 0), blue]);
        assert_eq!(&report[1..10], &[1, 255, 0, 0, 0x28, 255, 0, 0, 0x80]);
        assert_eq!(&report[31..40], &[1, 0, 255, 0, 0x28, 0, 255, 0, 0x80]);
        assert_eq!(report[41], 40, "LED count preserved");
        assert_eq!(&report[42..46], &[1, 0, 0, 255]);
        assert_eq!(report[52], 60);
        // Onboard zone: mirror of JRGB, JRGB/JPIPE sync off.
        assert_eq!(&report[74..78], &[1, 255, 0, 0]);
        assert_eq!(report[78], 0x29);
        assert_eq!(report[82], 0x81);
        assert_eq!(report[SAVE_FLAG_OFFSET], 0);
        assert_eq!(report[0], REPORT_ID);
        // Bytes outside zones untouched.
        assert_eq!(&report[94..174], &original[94..174]);
    }

    #[test]
    fn frame_validation() {
        let ok = vec![vec![Rgb::WHITE], vec![Rgb::BLACK, Rgb::WHITE], vec![Rgb::WHITE]];
        assert_eq!(frame_colors(&ok).unwrap()[1], Rgb::BLACK);
        assert!(frame_colors(&ok[..2]).is_err());
        assert!(frame_colors(&[vec![Rgb::WHITE], vec![], vec![Rgb::WHITE]]).is_err());
    }

    #[test]
    fn gate_rate_limits_and_skips_identical() {
        let mut gate = FrameGate::default();
        let start = Instant::now();
        let a = [1u8; REPORT_LEN];
        let b = [2u8; REPORT_LEN];
        assert!(gate.should_send(&a, start));
        gate.mark_sent(a, start);
        assert!(!gate.should_send(&b, start + Duration::from_millis(10)));
        assert!(!gate.should_send(&a, start + Duration::from_secs(1)));
        assert!(gate.should_send(&b, start + MIN_FRAME_INTERVAL));
        gate.reset();
        assert!(gate.should_send(&a, start));
    }
}
