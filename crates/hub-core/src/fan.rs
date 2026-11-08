//! Fan controller contract (Super I/O, GPU, hwmon, etc.).

use serde::{Deserialize, Serialize};

use crate::HwError;

/// Controllable PWM channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FanChannel {
    /// Stable id, e.g. `nct6687/pwm1`, `gpu0/fan0`.
    pub id: String,
    pub label: String,
    /// Id of the associated RPM `Reading`, if known.
    pub rpm_sensor: Option<String>,
}

/// Target applied to a channel (broadcast to the UI every tick).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FanStatus {
    pub channel: String,
    /// `None` = channel handed back to the firmware.
    pub duty: Option<u8>,
}

/// Fan hardware driver.
///
/// Implementations save the original state on open and call
/// `restore_all` in their `Drop`: a crash (unwind) or shutdown hands control back to the BIOS.
pub trait FanController: Send {
    fn name(&self) -> &str;
    fn channels(&self) -> &[FanChannel];
    /// Switches the channel to manual and sets the duty cycle (0..=100 %).
    fn set_duty(&mut self, channel: &str, percent: u8) -> Result<(), HwError>;
    /// Hands the channel back to the firmware (original mode).
    fn set_auto(&mut self, channel: &str) -> Result<(), HwError>;
    /// Hands every channel back to the firmware; must never panic.
    fn restore_all(&mut self);
}
