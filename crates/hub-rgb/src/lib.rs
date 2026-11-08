//! Native RGB lighting (no OpenRGB): effect engine + device drivers.

pub mod effects;
pub mod mystic_light;

use hub_core::rgb::RgbDevice;

/// Opens every supported RGB device present (read-only on open).
/// A missing or failing device is simply skipped (`warn` log).
pub fn detect() -> Vec<Box<dyn RgbDevice>> {
    mystic_light::detect()
}
