//! Generic driver for HID LampArray devices (Windows 11 "Dynamic Lighting" standard).
//!
//! Sources:
//! - USB-IF "HID Usage Tables" 1.4+, section "Lighting And Illumination Page (0x59)"
//!   (https://www.usb.org/sites/default/files/hut1_4.pdf).
//! - Microsoft "Dynamic Lighting devices"
//!   (https://learn.microsoft.com/windows-hardware/design/component-guidelines/dynamic-lighting-devices)
//!   and its reference firmware `microsoft/ArduinoHidForWindows` (MIT):
//!   `src/LampArrayReportDescriptor.h`, `src/Microsoft_HidLampArray.cpp`.
//!
//! Protocol facts used here:
//! - Application collection LampArray = page 0x59, usage 0x01. Every report below is a
//!   FEATURE report wrapped in a logical collection whose usage names it. Report ids
//!   are NOT fixed: they are read from the report descriptor, and so are the field
//!   sizes and bit offsets (the sample uses u16 LampId, u32 attributes, u8 channels).
//! - LampArrayAttributesReport (0x02): LampCount (0x03), BoundingBox Width/Height/Depth
//!   in micrometers (0x04..0x06), LampArrayKind (0x07), MinUpdateIntervalInMicroseconds
//!   (0x08). Read with GET_FEATURE.
//! - LampAttributesRequestReport (0x20): LampId (0x21), sets the attributes cursor.
//!   LampAttributesResponseReport (0x22): LampId, Position X/Y/Z (0x23..0x25),
//!   LampPurposes (0x26), UpdateLatency (0x27), Red/Green/Blue/Intensity level counts
//!   (0x28..0x2B), IsProgrammable (0x2C), InputBinding (0x2D). Each GET returns the lamp
//!   under the cursor, then the cursor moves to the next lamp.
//! - LampMultiUpdateReport (0x50): LampCount (0x03, lamps used in this report),
//!   LampUpdateFlags (0x55), N x LampId, then N x (Red 0x51, Green 0x52, Blue 0x53,
//!   Intensity 0x54) update channels. N = number of LampId fields in the descriptor.
//! - LampRangeUpdateReport (0x60): LampUpdateFlags, LampIdStart (0x61), LampIdEnd (0x62),
//!   one color for the inclusive range.
//! - LampUpdateFlags bit 0 = LampUpdateComplete: the device latches the batch only when
//!   set, so it goes on the last report of a frame only.
//! - LampArrayControlReport (0x70): AutonomousMode (0x71). 1 = the firmware drives its
//!   own effect (power-on default), 0 = the host drives the lamps. Updates are volatile
//!   by nature of the standard: nothing is ever saved to flash.
//!
//! Descriptor source: `hidapi::HidDevice::get_report_descriptor` (Linux: raw hidraw
//! descriptor; Windows: descriptor rebuilt by hidapi from the HIDP preparsed data).
//!
//! Dedicated drivers win: a VID:PID claimed by [`crate::has_dedicated_driver`] is skipped.
//!
//! Windows note: if "Dynamic Lighting" (Settings > Personalization > Dynamic Lighting,
//! "Use Dynamic Lighting on my devices") also drives the device, both hosts fight over
//! the lamps. Turn that switch off (or set this device's background control to another
//! app) before using this driver. Some vendors (e.g. Logitech G HUB) expose LampArray
//! through a virtual HID driver that only exists while their software runs.

use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice, MAX_REPORT_DESCRIPTOR_SIZE};
use hub_core::HwError;
use hub_core::rgb::{Rgb, RgbDevice, RgbDeviceInfo, RgbZone};

pub const USAGE_PAGE: u16 = 0x59;
pub const USAGE_LAMP_ARRAY: u16 = 0x01;

/// Usage ids of the Lighting And Illumination page (HUT 1.4, page 0x59).
pub mod usage {
    pub const ATTRIBUTES_REPORT: u16 = 0x02;
    pub const LAMP_COUNT: u16 = 0x03;
    pub const BOUNDING_BOX_WIDTH: u16 = 0x04;
    pub const BOUNDING_BOX_HEIGHT: u16 = 0x05;
    pub const BOUNDING_BOX_DEPTH: u16 = 0x06;
    pub const LAMP_ARRAY_KIND: u16 = 0x07;
    pub const MIN_UPDATE_INTERVAL: u16 = 0x08;
    pub const ATTRIBUTES_REQUEST_REPORT: u16 = 0x20;
    pub const LAMP_ID: u16 = 0x21;
    pub const ATTRIBUTES_RESPONSE_REPORT: u16 = 0x22;
    pub const MULTI_UPDATE_REPORT: u16 = 0x50;
    pub const RED_CHANNEL: u16 = 0x51;
    pub const GREEN_CHANNEL: u16 = 0x52;
    pub const BLUE_CHANNEL: u16 = 0x53;
    pub const INTENSITY_CHANNEL: u16 = 0x54;
    pub const UPDATE_FLAGS: u16 = 0x55;
    pub const RANGE_UPDATE_REPORT: u16 = 0x60;
    pub const LAMP_ID_START: u16 = 0x61;
    pub const LAMP_ID_END: u16 = 0x62;
    pub const CONTROL_REPORT: u16 = 0x70;
    pub const AUTONOMOUS_MODE: u16 = 0x71;
}

/// LampUpdateFlags bit 0.
pub const FLAG_UPDATE_COMPLETE: u32 = 1;
/// <= 30 frames/s, raised to the device MinUpdateInterval when larger.
pub const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(33);
/// GET_FEATURE buffer floor (Windows wants at least the largest feature report).
const GET_BUF_MIN: usize = 256;

/// One page-0x59 field of a feature report.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub usage: u16,
    /// Offset in bits after the report id byte.
    pub bit_offset: usize,
    pub bit_size: usize,
    /// Largest value to send (logical maximum, bounded by the field size).
    pub max: u32,
}

/// Feature report layout read from the descriptor.
#[derive(Debug, Clone, PartialEq)]
pub struct FeatureReport {
    pub id: u8,
    /// Usage of the enclosing page-0x59 logical collection (0 if none).
    pub collection: u16,
    pub fields: Vec<Field>,
    bits: usize,
}

impl FeatureReport {
    /// Buffer length for hidapi, report id byte included.
    pub fn buf_len(&self) -> usize {
        1 + self.bits.div_ceil(8)
    }

    /// `nth` field carrying `usage`.
    pub fn field(&self, usage: u16, nth: usize) -> Option<&Field> {
        self.fields.iter().filter(|field| field.usage == usage).nth(nth)
    }

    pub fn count(&self, usage: u16) -> usize {
        self.fields.iter().filter(|field| field.usage == usage).count()
    }

    /// Value of the first `usage` field in `data` (report id byte at index 0).
    pub fn get(&self, data: &[u8], usage: u16) -> Option<u32> {
        let data = data.get(1..).unwrap_or_default();
        self.field(usage, 0).map(|field| get_bits(data, field.bit_offset, field.bit_size))
    }

    /// Zeroed buffer with the report id.
    pub fn new_buf(&self) -> Vec<u8> {
        let mut buf = vec![0u8; self.buf_len()];
        buf[0] = self.id;
        buf
    }

    /// Writes `value` (clamped to the field maximum) into the `nth` `usage` field.
    pub fn set(&self, buf: &mut [u8], usage: u16, nth: usize, value: u32) -> Result<(), String> {
        let field = self
            .field(usage, nth)
            .ok_or_else(|| format!("report 0x{:02x} lacks usage 0x{usage:02x} #{nth}", self.id))?;
        set_bits(&mut buf[1..], field.bit_offset, field.bit_size, value.min(field.max));
        Ok(())
    }

    /// Scales an 8-bit channel to the field range (0..=max).
    fn set_channel(&self, buf: &mut [u8], usage: u16, nth: usize, level: u8) -> Result<(), String> {
        let max = self.field(usage, nth).map_or(0, |field| field.max);
        self.set(buf, usage, nth, u32::from(level) * max / 255)
    }

    fn set_color(&self, buf: &mut [u8], nth: usize, color: Rgb) -> Result<(), String> {
        self.set_channel(buf, usage::RED_CHANNEL, nth, color.r)?;
        self.set_channel(buf, usage::GREEN_CHANNEL, nth, color.g)?;
        self.set_channel(buf, usage::BLUE_CHANNEL, nth, color.b)?;
        // Intensity is optional in practice: full when present (brightness lives in the color).
        if self.count(usage::INTENSITY_CHANNEL) > nth {
            self.set(buf, usage::INTENSITY_CHANNEL, nth, u32::MAX)?;
        }
        Ok(())
    }

    fn require(&self, usages: &[u16], count: usize) -> Result<(), String> {
        match usages.iter().find(|&&usage| self.count(usage) < count) {
            Some(usage) => Err(format!("report 0x{:02x} lacks usage 0x{usage:02x}", self.id)),
            None => Ok(()),
        }
    }
}

fn get_bits(data: &[u8], offset: usize, size: usize) -> u32 {
    (0..size.min(32)).fold(0, |value, i| {
        let bit = offset + i;
        let set = data.get(bit / 8).is_some_and(|byte| (byte >> (bit % 8)) & 1 == 1);
        value | (u32::from(set) << i)
    })
}

fn set_bits(data: &mut [u8], offset: usize, size: usize, value: u32) {
    for i in 0..size.min(32) {
        let bit = offset + i;
        if let Some(byte) = data.get_mut(bit / 8) {
            let mask = 1 << (bit % 8);
            if (value >> i) & 1 == 1 { *byte |= mask } else { *byte &= !mask }
        }
    }
}

/// Global items that PUSH/POP save.
#[derive(Clone, Copy, Default)]
struct Globals {
    usage_page: u16,
    logical_max: u32,
    report_size: usize,
    report_count: usize,
    report_id: u8,
}

/// Parses the feature reports of a HID report descriptor (HID 1.11, section 6.2.2).
/// Only page-0x59 fields are kept; other fields still advance the bit offsets.
pub fn parse_descriptor(desc: &[u8]) -> Result<Vec<FeatureReport>, String> {
    let mut reports: Vec<FeatureReport> = Vec::new();
    let mut globals = Globals::default();
    let mut stack: Vec<Globals> = Vec::new();
    let mut usages: Vec<u32> = Vec::new();
    let mut usage_min = 0u32;
    let mut collections: Vec<u32> = Vec::new();
    let mut pos = 0;
    while pos < desc.len() {
        let prefix = desc[pos];
        if prefix == 0xFE {
            // Long item: never used by LampArray, skipped.
            let size = usize::from(*desc.get(pos + 1).ok_or("truncated long item")?);
            pos += 3 + size;
            continue;
        }
        let size = [0, 1, 2, 4][usize::from(prefix & 3)];
        let bytes = desc.get(pos + 1..pos + 1 + size).ok_or("truncated descriptor")?;
        let value = bytes.iter().rev().fold(0u32, |acc, byte| acc << 8 | u32::from(*byte));
        pos += 1 + size;
        let page = u32::from(globals.usage_page);
        let full_usage = move |value: u32| if size == 4 { value } else { page << 16 | value };
        match prefix & 0xFC {
            // Main items.
            0xA0 => collections.push(usages.first().copied().unwrap_or(0)),
            0xC0 => {
                collections.pop();
            }
            0xB0 => {
                let index = match reports.iter().position(|report| report.id == globals.report_id) {
                    Some(index) => index,
                    None => {
                        let collection = collections.last().copied().unwrap_or(0);
                        let collection =
                            if collection >> 16 == u32::from(USAGE_PAGE) { collection as u16 } else { 0 };
                        reports.push(FeatureReport {
                            id: globals.report_id,
                            collection,
                            fields: Vec::new(),
                            bits: 0,
                        });
                        reports.len() - 1
                    }
                };
                let report = &mut reports[index];
                let bit_max = if globals.report_size >= 32 {
                    u32::MAX
                } else {
                    (1u32 << globals.report_size) - 1
                };
                let max = if globals.logical_max == 0 { bit_max } else { globals.logical_max.min(bit_max) };
                for i in 0..globals.report_count {
                    // Fields beyond the usage list reuse the last usage; none = padding.
                    let usage = usages.get(i).or(usages.last());
                    if let Some(&usage) = usage.filter(|&&usage| usage >> 16 == u32::from(USAGE_PAGE)) {
                        report.fields.push(Field {
                            usage: usage as u16,
                            bit_offset: report.bits,
                            bit_size: globals.report_size,
                            max,
                        });
                    }
                    report.bits += globals.report_size;
                }
            }
            0x80 | 0x90 => {}
            // Global items.
            0x04 => globals.usage_page = value as u16,
            0x24 => globals.logical_max = value,
            0x74 => globals.report_size = value as usize,
            0x84 => globals.report_id = value as u8,
            0x94 => globals.report_count = value as usize,
            0xA4 => stack.push(globals),
            0xB4 => globals = stack.pop().ok_or("POP without PUSH")?,
            // Local items.
            0x08 => usages.push(full_usage(value)),
            0x18 => usage_min = full_usage(value),
            0x28 => {
                let usage_max = full_usage(value);
                if usage_max.saturating_sub(usage_min) > 0xFFFF {
                    return Err("usage range too large".into());
                }
                usages.extend(usage_min..=usage_max);
            }
            _ => {}
        }
        // Local items only live until the next main item.
        if prefix & 0x0C == 0 {
            usages.clear();
        }
    }
    Ok(reports)
}

/// The LampArray reports of a device.
#[derive(Debug, Clone)]
pub struct Layout {
    pub attributes: FeatureReport,
    pub multi_update: FeatureReport,
    pub range_update: Option<FeatureReport>,
    pub control: FeatureReport,
    pub attributes_request: Option<FeatureReport>,
    pub attributes_response: Option<FeatureReport>,
    /// Largest feature report (GET buffer size).
    pub max_len: usize,
}

impl Layout {
    pub fn from_descriptor(desc: &[u8]) -> Result<Self, String> {
        let reports = parse_descriptor(desc)?;
        let find = |collection: u16| reports.iter().find(|report| report.collection == collection).cloned();
        let need = |collection: u16, name: &str| {
            find(collection).ok_or_else(|| format!("no {name} in the report descriptor"))
        };
        let attributes = need(usage::ATTRIBUTES_REPORT, "LampArrayAttributesReport")?;
        attributes.require(&[usage::LAMP_COUNT], 1)?;
        let multi_update = need(usage::MULTI_UPDATE_REPORT, "LampMultiUpdateReport")?;
        let slots = multi_update.count(usage::LAMP_ID);
        if slots == 0 {
            return Err("LampMultiUpdateReport has no LampId slot".into());
        }
        multi_update.require(&[usage::LAMP_COUNT, usage::UPDATE_FLAGS], 1)?;
        multi_update.require(&[usage::RED_CHANNEL, usage::GREEN_CHANNEL, usage::BLUE_CHANNEL], slots)?;
        let range_update = find(usage::RANGE_UPDATE_REPORT).filter(|report| {
            let channels = [usage::RED_CHANNEL, usage::GREEN_CHANNEL, usage::BLUE_CHANNEL];
            let ids = [usage::UPDATE_FLAGS, usage::LAMP_ID_START, usage::LAMP_ID_END];
            report.require(&channels, 1).is_ok() && report.require(&ids, 1).is_ok()
        });
        let control = need(usage::CONTROL_REPORT, "LampArrayControlReport")?;
        control.require(&[usage::AUTONOMOUS_MODE], 1)?;
        Ok(Self {
            attributes,
            multi_update,
            range_update,
            control,
            attributes_request: find(usage::ATTRIBUTES_REQUEST_REPORT),
            attributes_response: find(usage::ATTRIBUTES_RESPONSE_REPORT),
            max_len: reports.iter().map(FeatureReport::buf_len).max().unwrap_or(0),
        })
    }
}

/// Decoded LampArrayAttributesReport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attributes {
    pub lamp_count: u16,
    pub bounding_box_um: [u32; 3],
    pub kind: u32,
    pub min_update_interval_us: u32,
}

/// Validates and decodes a LampArrayAttributesReport (`read` = bytes from GET_FEATURE).
pub fn parse_attributes(report: &FeatureReport, read: &[u8]) -> Result<Attributes, String> {
    if read.len() < report.buf_len() {
        return Err(format!("attributes: {} bytes read, {} expected", read.len(), report.buf_len()));
    }
    if read[0] != report.id {
        return Err(format!("attributes: unexpected report id 0x{:02x}", read[0]));
    }
    let get = |usage| report.get(read, usage).unwrap_or(0);
    let lamp_count = get(usage::LAMP_COUNT);
    if lamp_count == 0 || lamp_count > u32::from(u16::MAX) {
        return Err(format!("invalid LampCount {lamp_count}"));
    }
    Ok(Attributes {
        lamp_count: lamp_count as u16,
        bounding_box_um: [
            get(usage::BOUNDING_BOX_WIDTH),
            get(usage::BOUNDING_BOX_HEIGHT),
            get(usage::BOUNDING_BOX_DEPTH),
        ],
        kind: get(usage::LAMP_ARRAY_KIND),
        min_update_interval_us: get(usage::MIN_UPDATE_INTERVAL),
    })
}

/// LampArrayControlReport with AutonomousMode set to `autonomous`.
pub fn build_control(control: &FeatureReport, autonomous: bool) -> Result<Vec<u8>, String> {
    let mut buf = control.new_buf();
    control.set(&mut buf, usage::AUTONOMOUS_MODE, 0, u32::from(autonomous))?;
    Ok(buf)
}

/// Reports that move the lamps from `previous` (None = unknown) to `colors`.
/// Only changed lamps are sent; one range report when they form a single-color run,
/// otherwise multi-update batches. LampUpdateComplete is set on the last report only.
pub fn build_updates(
    layout: &Layout,
    previous: Option<&[Rgb]>,
    colors: &[Rgb],
) -> Result<Vec<Vec<u8>>, String> {
    let changed: Vec<usize> = (0..colors.len())
        .filter(|&lamp| previous.is_none_or(|previous| previous.get(lamp) != Some(&colors[lamp])))
        .collect();
    let (Some(&first), Some(&last)) = (changed.first(), changed.last()) else {
        return Ok(Vec::new());
    };
    if let Some(range) = &layout.range_update {
        let contiguous = last - first + 1 == changed.len();
        if contiguous && changed.iter().all(|&lamp| colors[lamp] == colors[first]) {
            let mut buf = range.new_buf();
            range.set(&mut buf, usage::UPDATE_FLAGS, 0, FLAG_UPDATE_COMPLETE)?;
            range.set(&mut buf, usage::LAMP_ID_START, 0, first as u32)?;
            range.set(&mut buf, usage::LAMP_ID_END, 0, last as u32)?;
            range.set_color(&mut buf, 0, colors[first])?;
            return Ok(vec![buf]);
        }
    }
    let multi = &layout.multi_update;
    let batches: Vec<&[usize]> = changed.chunks(multi.count(usage::LAMP_ID)).collect();
    let mut reports = Vec::with_capacity(batches.len());
    for (index, batch) in batches.iter().enumerate() {
        let mut buf = multi.new_buf();
        multi.set(&mut buf, usage::LAMP_COUNT, 0, batch.len() as u32)?;
        let flags = if index + 1 == batches.len() { FLAG_UPDATE_COMPLETE } else { 0 };
        multi.set(&mut buf, usage::UPDATE_FLAGS, 0, flags)?;
        for (slot, &lamp) in batch.iter().enumerate() {
            multi.set(&mut buf, usage::LAMP_ID, slot, lamp as u32)?;
            multi.set_color(&mut buf, slot, colors[lamp])?;
        }
        reports.push(buf);
    }
    Ok(reports)
}

/// Reads and parses the LampArray layout (read-only).
pub fn read_layout(device: &HidDevice) -> Result<Layout, String> {
    let mut desc = vec![0u8; MAX_REPORT_DESCRIPTOR_SIZE];
    let len = device.get_report_descriptor(&mut desc).map_err(|e| e.to_string())?;
    Layout::from_descriptor(&desc[..len.min(desc.len())])
}

/// GET_FEATURE of `report` (read-only).
pub fn get_feature(device: &HidDevice, layout: &Layout, report: &FeatureReport) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; layout.max_len.max(GET_BUF_MIN)];
    buf[0] = report.id;
    let len = device.get_feature_report(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(len);
    Ok(buf)
}

/// Reads the attributes of `lamp_id`: SET LampAttributesRequest (moves the attributes
/// cursor only, no lighting change) then GET LampAttributesResponse.
pub fn read_lamp_attributes(device: &HidDevice, layout: &Layout, lamp_id: u16) -> Result<Vec<u8>, String> {
    let (Some(request), Some(response)) = (&layout.attributes_request, &layout.attributes_response) else {
        return Err("no LampAttributes request/response reports".into());
    };
    let mut buf = request.new_buf();
    request.set(&mut buf, usage::LAMP_ID, 0, u32::from(lamp_id))?;
    device.send_feature_report(&buf).map_err(|e| e.to_string())?;
    get_feature(device, layout, response)
}

pub struct LampArray {
    info: RgbDeviceInfo,
    device: HidDevice,
    layout: Layout,
    lamp_count: usize,
    frame_interval: Duration,
    last_sent: Option<(Instant, Vec<Rgb>)>,
    /// True once AutonomousMode=0 may have been sent: `restore` only writes then.
    host_mode: bool,
}

impl LampArray {
    fn open(api: &HidApi, info: &hidapi::DeviceInfo, id: String) -> Result<Self, String> {
        let device = api.open_path(info.path()).map_err(|e| e.to_string())?;
        let layout = read_layout(&device)?;
        let attributes = parse_attributes(&layout.attributes, &get_feature(&device, &layout, &layout.attributes)?)?;
        let lamp_count = usize::from(attributes.lamp_count);
        let label = device_label(info.product_string(), info.manufacturer_string(), info.vendor_id(), attributes.kind);
        let info = RgbDeviceInfo {
            id,
            label,
            zones: vec![RgbZone { id: "lamps".into(), label: "Lamps".into(), leds: lamp_count }],
        };
        let frame_interval =
            MIN_FRAME_INTERVAL.max(Duration::from_micros(u64::from(attributes.min_update_interval_us)));
        Ok(Self { info, device, layout, lamp_count, frame_interval, last_sent: None, host_mode: false })
    }

    fn send(&self, report: &[u8]) -> Result<(), HwError> {
        self.device
            .send_feature_report(report)
            .map_err(|e| HwError::Unavailable(format!("LampArray {}: {e}", self.info.id)))
    }

    fn send_all(&mut self, colors: &[Rgb]) -> Result<(), HwError> {
        if !self.host_mode {
            self.host_mode = true;
            self.send(&build_control(&self.layout.control, false).map_err(HwError::Other)?)?;
        }
        let previous = self.last_sent.as_ref().map(|(_, colors)| colors.as_slice());
        for report in build_updates(&self.layout, previous, colors).map_err(HwError::Other)? {
            self.send(&report)?;
        }
        Ok(())
    }
}

impl RgbDevice for LampArray {
    fn info(&self) -> &RgbDeviceInfo {
        &self.info
    }

    fn set_frame(&mut self, frame: &[Vec<Rgb>]) -> Result<(), HwError> {
        let colors = match frame {
            [colors] if colors.len() == self.lamp_count => colors,
            _ => return Err(HwError::Other(format!("frame does not match {} lamps", self.lamp_count))),
        };
        let now = Instant::now();
        if let Some((at, last)) = &self.last_sent
            && (last == colors || now.duration_since(*at) < self.frame_interval)
        {
            return Ok(());
        }
        match self.send_all(colors) {
            Ok(()) => {
                self.last_sent = Some((now, colors.clone()));
                Ok(())
            }
            Err(e) => {
                // Partial batch: the device state is unknown, resend everything next time.
                self.last_sent = None;
                Err(e)
            }
        }
    }

    fn restore(&mut self) {
        if !self.host_mode {
            return;
        }
        let result = build_control(&self.layout.control, true).map_err(HwError::Other);
        match result.and_then(|report| self.send(&report)) {
            Ok(()) => self.host_mode = false,
            Err(e) => tracing::warn!("LampArray restore failed: {e}"),
        }
        self.last_sent = None;
    }
}

impl Drop for LampArray {
    fn drop(&mut self) {
        self.restore();
    }
}

/// HID string set by a generic driver (e.g. "HID VHF Driver" on Logitech), useless as a name.
fn is_generic(text: Option<&str>) -> bool {
    text.map(str::trim).is_none_or(|text| text.is_empty() || text.contains("VHF") || text.contains("Microsoft"))
}

/// LampArrayKind name (HID Usage Tables, page 0x59, usage 0x07).
fn kind_name(kind: u32) -> &'static str {
    match kind {
        1 => "Keyboard",
        2 => "Mouse",
        3 => "Game Controller",
        4 => "Peripheral",
        5 => "Scene",
        6 => "Notification",
        7 => "Chassis",
        8 => "Wearable",
        9 => "Furniture",
        10 => "Art",
        _ => "Lighting",
    }
}

/// Display name: the HID product string, or "{vendor} {kind}" when it is generic.
fn device_label(product: Option<&str>, manufacturer: Option<&str>, vid: u16, kind: u32) -> String {
    if let Some(product) = product.filter(|_| !is_generic(product)) {
        return product.trim().to_string();
    }
    let vendor = match (manufacturer.filter(|_| !is_generic(manufacturer)), vid) {
        (Some(name), _) => name.trim().to_string(),
        (None, 0x046d) => "Logitech".into(),
        (None, 0x1532) => "Razer".into(),
        (None, 0x1b1c) => "Corsair".into(),
        (None, 0x0b05) => "ASUS".into(),
        (None, 0x1038) => "SteelSeries".into(),
        (None, 0x0951 | 0x03f0) => "HyperX".into(),
        (None, vid) => format!("VID {vid:04x}"),
    };
    format!("{vendor} {}", kind_name(kind))
}

/// True for a HID interface exposing the LampArray application collection.
pub fn is_lamp_array(info: &hidapi::DeviceInfo) -> bool {
    info.usage_page() == USAGE_PAGE && info.usage() == USAGE_LAMP_ARRAY
}

/// Stable id: `lamparray/{vid}:{pid}/{serial or interface number}`.
pub fn device_id(info: &hidapi::DeviceInfo) -> String {
    let suffix = match info.serial_number().map(str::trim) {
        Some(serial) if !serial.is_empty() => serial.to_string(),
        _ => format!("if{}", info.interface_number()),
    };
    format!("lamparray/{:04x}:{:04x}/{suffix}", info.vendor_id(), info.product_id())
}

/// Opens every LampArray interface not claimed by a dedicated driver; no write.
pub fn detect() -> Vec<Box<dyn RgbDevice>> {
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            tracing::warn!("hidapi unavailable: {e}");
            return Vec::new();
        }
    };
    let mut found: Vec<Box<dyn RgbDevice>> = Vec::new();
    for info in api.device_list().filter(|info| is_lamp_array(info)) {
        if crate::has_dedicated_driver(info.vendor_id(), info.product_id()) {
            continue;
        }
        let id = device_id(info);
        if found.iter().any(|device| device.info().id == id) {
            continue;
        }
        match LampArray::open(&api, info, id.clone()) {
            Ok(device) => found.push(Box::new(device)),
            Err(e) => tracing::warn!("{id} skipped ({:?}): {e}", info.path()),
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference descriptor of the Microsoft LampArray sample (report ids 1..=6,
    /// u16 LampCount, 5 x u32 attributes, 8 multi-update slots, u8 channels).
    const SAMPLE_DESCRIPTOR: &[u8] = &[
        0x05, 0x59, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x01, 0x09, 0x02, 0xa1, 0x02, 0x09, 0x03, 0x15,
        0x00, 0x27, 0xff, 0xff, 0x00, 0x00, 0x95, 0x01, 0x75, 0x10, 0xb1, 0x03, 0x09, 0x04, 0x09,
        0x05, 0x09, 0x06, 0x09, 0x07, 0x09, 0x08, 0x27, 0xff, 0xff, 0xff, 0x7f, 0x95, 0x05, 0x75,
        0x20, 0xb1, 0x03, 0xc0, 0x85, 0x02, 0x09, 0x20, 0xa1, 0x02, 0x09, 0x21, 0x27, 0xff, 0xff,
        0x00, 0x00, 0x95, 0x01, 0x75, 0x10, 0xb1, 0x02, 0xc0, 0x85, 0x03, 0x09, 0x22, 0xa1, 0x02,
        0x09, 0x21, 0xb1, 0x02, 0x09, 0x23, 0x09, 0x24, 0x09, 0x25, 0x09, 0x27, 0x09, 0x26, 0x27,
        0xff, 0xff, 0xff, 0x7f, 0x95, 0x05, 0x75, 0x20, 0xb1, 0x02, 0x09, 0x28, 0x09, 0x29, 0x09,
        0x2a, 0x09, 0x2b, 0x09, 0x2c, 0x09, 0x2d, 0x26, 0xff, 0x00, 0x95, 0x06, 0x75, 0x08, 0xb1,
        0x02, 0xc0, 0x85, 0x04, 0x09, 0x50, 0xa1, 0x02, 0x09, 0x03, 0x25, 0x08, 0x95, 0x01, 0xb1,
        0x02, 0x09, 0x55, 0x25, 0x01, 0xb1, 0x02, 0x09, 0x21, 0x27, 0xff, 0xff, 0x00, 0x00, 0x95,
        0x08, 0x75, 0x10, 0xb1, 0x02, 0x09, 0x51, 0x09, 0x52, 0x09, 0x53, 0x09, 0x54, 0x09, 0x51,
        0x09, 0x52, 0x09, 0x53, 0x09, 0x54, 0x09, 0x51, 0x09, 0x52, 0x09, 0x53, 0x09, 0x54, 0x09,
        0x51, 0x09, 0x52, 0x09, 0x53, 0x09, 0x54, 0x09, 0x51, 0x09, 0x52, 0x09, 0x53, 0x09, 0x54,
        0x09, 0x51, 0x09, 0x52, 0x09, 0x53, 0x09, 0x54, 0x09, 0x51, 0x09, 0x52, 0x09, 0x53, 0x09,
        0x54, 0x09, 0x51, 0x09, 0x52, 0x09, 0x53, 0x09, 0x54, 0x26, 0xff, 0x00, 0x95, 0x20, 0x75,
        0x08, 0xb1, 0x02, 0xc0, 0x85, 0x05, 0x09, 0x60, 0xa1, 0x02, 0x09, 0x55, 0x25, 0x01, 0x95,
        0x01, 0xb1, 0x02, 0x09, 0x61, 0x09, 0x62, 0x27, 0xff, 0xff, 0x00, 0x00, 0x95, 0x02, 0x75,
        0x10, 0xb1, 0x02, 0x09, 0x51, 0x09, 0x52, 0x09, 0x53, 0x09, 0x54, 0x26, 0xff, 0x00, 0x95,
        0x04, 0x75, 0x08, 0xb1, 0x02, 0xc0, 0x85, 0x06, 0x09, 0x70, 0xa1, 0x02, 0x09, 0x71, 0x25,
        0x01, 0x95, 0x01, 0xb1, 0x02, 0xc0, 0xc0,
    ];

    fn layout() -> Layout {
        Layout::from_descriptor(SAMPLE_DESCRIPTOR).expect("sample descriptor")
    }

    const RED: Rgb = Rgb::new(255, 0, 0);
    const BLUE: Rgb = Rgb::new(0, 0, 255);

    #[test]
    fn generic_product_names_use_vendor_and_kind() {
        assert_eq!(device_label(Some("HID VHF Driver"), Some("Logitech"), 0x046d, 2), "Logitech Mouse");
        assert_eq!(device_label(Some("HID VHF Driver"), None, 0x046d, 1), "Logitech Keyboard");
        assert_eq!(device_label(Some(" "), Some("Microsoft"), 0x1532, 3), "Razer Game Controller");
        assert_eq!(device_label(None, Some("Acme"), 0x1234, 7), "Acme Chassis");
        assert_eq!(device_label(None, None, 0x1234, 0), "VID 1234 Lighting");
        assert_eq!(device_label(Some("Sample Lamp "), None, 0x1234, 1), "Sample Lamp");
    }

    #[test]
    fn descriptor_report_ids_and_sizes() {
        let layout = layout();
        let ids_and_lens = [
            (layout.attributes.id, layout.attributes.buf_len()),
            (layout.multi_update.id, layout.multi_update.buf_len()),
            (layout.control.id, layout.control.buf_len()),
        ];
        // Sizes from the sample header (22, 50, 1 data bytes) + report id byte.
        assert_eq!(ids_and_lens, [(1, 23), (4, 51), (6, 2)]);
        assert_eq!(layout.range_update.as_ref().map(|r| (r.id, r.buf_len())), Some((5, 10)));
        assert_eq!(layout.attributes_request.as_ref().map(|r| r.id), Some(2));
        assert_eq!(layout.attributes_response.as_ref().map(|r| (r.id, r.buf_len())), Some((3, 29)));
        assert_eq!(layout.max_len, 51);
        assert_eq!(layout.multi_update.count(usage::LAMP_ID), 8);
        assert_eq!(layout.multi_update.count(usage::RED_CHANNEL), 8);
        let red3 = layout.multi_update.field(usage::RED_CHANNEL, 3).expect("red #3");
        assert_eq!((red3.bit_offset, red3.bit_size, red3.max), (8 * (2 + 16 + 12), 8, 255));
    }

    #[test]
    fn rejects_non_lamparray_descriptors() {
        // Mouse-like descriptor: no page-0x59 reports.
        let mouse = [0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x95, 0x03, 0x75, 0x08, 0x81, 0x02, 0xc0];
        assert!(Layout::from_descriptor(&mouse).is_err());
        assert!(parse_descriptor(&[0x27, 0xff]).is_err(), "truncated item");
        // Without the control report the device cannot be handed back: rejected.
        let cut = SAMPLE_DESCRIPTOR.len() - 14;
        assert!(Layout::from_descriptor(&SAMPLE_DESCRIPTOR[..cut]).is_err());
    }

    #[test]
    fn parses_attributes() {
        let layout = layout();
        let mut read = vec![1u8];
        read.extend_from_slice(&26u16.to_le_bytes());
        for value in [100_000u32, 50_000, 10_000, 1, 10_000] {
            read.extend_from_slice(&value.to_le_bytes());
        }
        let attributes = parse_attributes(&layout.attributes, &read).expect("attributes");
        assert_eq!(
            attributes,
            Attributes {
                lamp_count: 26,
                bounding_box_um: [100_000, 50_000, 10_000],
                kind: 1,
                min_update_interval_us: 10_000
            }
        );
        // Windows may return the largest report length: extra bytes accepted.
        read.resize(layout.max_len, 0);
        assert!(parse_attributes(&layout.attributes, &read).is_ok());
        assert!(parse_attributes(&layout.attributes, &read[..10]).is_err());
        let mut wrong_id = read.clone();
        wrong_id[0] = 2;
        assert!(parse_attributes(&layout.attributes, &wrong_id).is_err());
        let mut no_lamp = read;
        no_lamp[1] = 0;
        assert!(parse_attributes(&layout.attributes, &no_lamp).is_err());
    }

    #[test]
    fn control_report() {
        let layout = layout();
        assert_eq!(build_control(&layout.control, false).expect("control"), [6, 0]);
        assert_eq!(build_control(&layout.control, true).expect("control"), [6, 1]);
    }

    #[test]
    fn uniform_frame_uses_one_range_report() {
        let reports = build_updates(&layout(), None, &[RED; 20]).expect("updates");
        assert_eq!(reports, [vec![5, 1, 0, 0, 19, 0, 255, 0, 0, 255]]);
    }

    #[test]
    fn mixed_frame_uses_multi_update_batches() {
        let colors: Vec<Rgb> = (0..10).map(|lamp| if lamp % 2 == 0 { RED } else { BLUE }).collect();
        let reports = build_updates(&layout(), None, &colors).expect("updates");
        assert_eq!(reports.len(), 2);
        let (first, second) = (&reports[0], &reports[1]);
        assert_eq!(first.len(), 51);
        // id, LampCount 8, flags 0 (batch not complete), LampIds 0..8.
        assert_eq!(&first[..5], &[4, 8, 0, 0, 0]);
        assert_eq!(&first[17..19], &[7, 0]);
        // Slot 0 red, slot 1 blue (R, G, B, I).
        assert_eq!(&first[19..27], &[255, 0, 0, 255, 0, 0, 255, 255]);
        // Last batch: 2 lamps (8, 9), LampUpdateComplete set, unused slots zeroed.
        assert_eq!(&second[..7], &[4, 2, 1, 8, 0, 9, 0]);
        assert_eq!(&second[19..27], &[255, 0, 0, 255, 0, 0, 255, 255]);
        assert!(second[27..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn only_changed_lamps_are_sent() {
        let layout = layout();
        let before = vec![RED; 12];
        assert!(build_updates(&layout, Some(&before), &before).expect("updates").is_empty());
        let mut after = before.clone();
        after[3] = BLUE;
        after[9] = BLUE;
        // Same color but not contiguous: one multi-update with two lamps.
        let reports = build_updates(&layout, Some(&before), &after).expect("updates");
        assert_eq!(reports.len(), 1);
        assert_eq!(&reports[0][..7], &[4, 2, 1, 3, 0, 9, 0]);
        // Contiguous run of one color: range report.
        after[4..9].fill(BLUE);
        let reports = build_updates(&layout, Some(&before), &after).expect("updates");
        assert_eq!(reports, [vec![5, 1, 3, 0, 9, 0, 0, 0, 255, 255]]);
    }

    #[test]
    fn channels_scale_to_logical_max() {
        // Range report variant with 4-bit channels (logical max 15).
        let report = FeatureReport {
            id: 9,
            collection: usage::RANGE_UPDATE_REPORT,
            fields: [usage::RED_CHANNEL, usage::GREEN_CHANNEL, usage::BLUE_CHANNEL]
                .iter()
                .enumerate()
                .map(|(i, &usage)| Field { usage, bit_offset: i * 4, bit_size: 4, max: 15 })
                .collect(),
            bits: 12,
        };
        let mut buf = report.new_buf();
        report.set_color(&mut buf, 0, Rgb::new(255, 128, 0)).expect("color");
        assert_eq!(buf, [9, 0x7f, 0x00]);
    }
}
