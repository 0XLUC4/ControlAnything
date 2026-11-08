//! Contrat des contrôleurs de ventilos (Super I/O, GPU, hwmon…).

use serde::{Deserialize, Serialize};

use crate::HwError;

/// Canal PWM pilotable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FanChannel {
    /// Id stable, ex. `nct6687/pwm1`, `gpu0/fan0`.
    pub id: String,
    pub label: String,
    /// Id du `Reading` RPM associé, s'il est connu.
    pub rpm_sensor: Option<String>,
}

/// Consigne appliquée à un canal (diffusée à l'UI à chaque tick).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FanStatus {
    pub channel: String,
    /// `None` = canal rendu au firmware.
    pub duty: Option<u8>,
}

/// Pilote matériel de ventilos.
///
/// Les implémentations sauvegardent l'état d'origine à l'ouverture et appellent
/// `restore_all` dans leur `Drop` : plantage (unwind) ou arrêt rendent la main au BIOS.
pub trait FanController: Send {
    fn name(&self) -> &str;
    fn channels(&self) -> &[FanChannel];
    /// Passe le canal en manuel et fixe le rapport cyclique (0..=100 %).
    fn set_duty(&mut self, channel: &str, percent: u8) -> Result<(), HwError>;
    /// Rend le canal au firmware (mode d'origine).
    fn set_auto(&mut self, channel: &str) -> Result<(), HwError>;
    /// Rend tous les canaux au firmware ; ne doit jamais paniquer.
    fn restore_all(&mut self);
}
