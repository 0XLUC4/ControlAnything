//! Native RGB lighting (no OpenRGB): effect engine + device drivers.

pub mod effects;
pub mod lamparray;
pub mod mystic_light;

use hub_core::rgb::RgbDevice;

/// Opens every supported RGB device present (read-only on open).
/// A missing or failing device is simply skipped (`warn` log).
pub fn detect() -> Vec<Box<dyn RgbDevice>> {
    let mut devices = mystic_light::detect();
    devices.extend(lamparray::detect());
    devices
}

/// Dedicated drivers win over the generic LampArray driver: a VID:PID listed here is
/// never opened by [`lamparray`], so one device is never driven twice.
pub fn has_dedicated_driver(vid: u16, pid: u16) -> bool {
    mystic_light::is_allowed(vid, pid)
}
